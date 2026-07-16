#!/usr/bin/env node

import { spawnSync } from 'node:child_process';
import {
  mkdtempSync,
  mkdirSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import process from 'node:process';
import { fileURLToPath } from 'node:url';

const workspaceRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const policyPath = path.join(workspaceRoot, '.ranvier-coverage-policy.json');

function fail(message) {
  throw new Error(`[coverage-gate] ${message}`);
}

function readJson(filePath, label) {
  let text;
  try {
    text = readFileSync(filePath, 'utf8');
  } catch (error) {
    fail(`${label} is missing or unreadable at ${filePath}: ${error.message}`);
  }

  try {
    return JSON.parse(text);
  } catch (error) {
    fail(`${label} is not valid JSON at ${filePath}: ${error.message}`);
  }
}

function requireNonEmptyString(value, label) {
  if (typeof value !== 'string' || value.trim() === '') {
    fail(`${label} must be a non-empty string`);
  }
  return value;
}

function requireFiniteNumber(value, label) {
  if (typeof value !== 'number' || !Number.isFinite(value)) {
    fail(`${label} must be a finite number`);
  }
  return value;
}

function validateMetric(metric, label) {
  if (metric == null || typeof metric !== 'object' || Array.isArray(metric)) {
    fail(`${label} must be an object`);
  }
  const count = requireFiniteNumber(metric.count, `${label}.count`);
  const covered = requireFiniteNumber(metric.covered, `${label}.covered`);
  const percent = requireFiniteNumber(metric.percent, `${label}.percent`);
  if (!Number.isInteger(count) || count <= 0) {
    fail(`${label}.count must be a positive integer`);
  }
  if (!Number.isInteger(covered) || covered < 0 || covered > count) {
    fail(`${label}.covered must be an integer between zero and count`);
  }
  if (percent < 0 || percent > 100) {
    fail(`${label}.percent must be between zero and 100`);
  }
  const calculated = (covered / count) * 100;
  if (Math.abs(calculated - percent) > 0.000001) {
    fail(`${label}.percent ${percent} does not match ${covered}/${count}`);
  }
  return { count, covered, percent };
}

function validatePolicy(raw) {
  if (raw == null || typeof raw !== 'object' || Array.isArray(raw)) {
    fail('coverage policy must be an object');
  }
  requireNonEmptyString(raw.schema_version, 'policy.schema_version');
  requireNonEmptyString(raw.engine?.name, 'policy.engine.name');
  requireNonEmptyString(raw.engine?.version, 'policy.engine.version');
  requireNonEmptyString(raw.engine?.toolchain, 'policy.engine.toolchain');
  requireNonEmptyString(raw.engine?.component, 'policy.engine.component');
  if (raw.engine.name !== 'cargo-llvm-cov') {
    fail(`unsupported coverage engine ${raw.engine.name}`);
  }

  const packages = raw.scope?.packages;
  if (!Array.isArray(packages) || packages.length === 0) {
    fail('policy.scope.packages must be a non-empty array');
  }
  packages.forEach((item, index) => requireNonEmptyString(item, `policy.scope.packages[${index}]`));
  if (new Set(packages).size !== packages.length) {
    fail('policy.scope.packages must not contain duplicates');
  }
  if (raw.scope.all_features !== true || raw.scope.locked !== true) {
    fail('coverage scope must keep all_features and locked enabled');
  }
  if (JSON.stringify(raw.scope.target_kinds) !== JSON.stringify(['lib', 'tests'])) {
    fail('coverage target kinds must remain exactly lib and tests');
  }
  if (!Array.isArray(raw.scope.exclusions) || raw.scope.exclusions.length === 0) {
    fail('policy.scope.exclusions must be explicit');
  }
  if (!Array.isArray(raw.scope.required_services) || raw.scope.required_services.length === 0) {
    fail('policy.scope.required_services must be explicit');
  }
  raw.scope.required_services.forEach((entry, index) =>
    requireNonEmptyString(entry, `policy.scope.required_services[${index}]`),
  );
  raw.scope.exclusions.forEach((entry, index) => {
    requireNonEmptyString(entry?.surface, `policy.scope.exclusions[${index}].surface`);
    requireNonEmptyString(entry?.owner, `policy.scope.exclusions[${index}].owner`);
    requireNonEmptyString(entry?.reason, `policy.scope.exclusions[${index}].reason`);
  });

  const baseline = {
    lines: validateMetric(raw.baseline?.lines, 'policy.baseline.lines'),
    regions: validateMetric(raw.baseline?.regions, 'policy.baseline.regions'),
    functions: validateMetric(raw.baseline?.functions, 'policy.baseline.functions'),
    instantiations: validateMetric(raw.baseline?.instantiations, 'policy.baseline.instantiations'),
  };
  const minimumPercent = requireFiniteNumber(
    raw.gate?.minimum_percent,
    'policy.gate.minimum_percent',
  );
  if (raw.gate?.blocking_metric !== 'lines') {
    fail('policy.gate.blocking_metric must be lines');
  }
  if (minimumPercent <= 0 || minimumPercent > baseline.lines.percent) {
    fail('line threshold must be positive and no higher than the measured baseline');
  }
  if (
    !Array.isArray(raw.gate.threshold_change_requires) ||
    raw.gate.threshold_change_requires.length === 0
  ) {
    fail('policy.gate.threshold_change_requires must be explicit');
  }
  raw.gate.threshold_change_requires.forEach((entry, index) =>
    requireNonEmptyString(entry, `policy.gate.threshold_change_requires[${index}]`),
  );

  return { raw, packages, baseline, minimumPercent };
}

function coverageTotals(report) {
  const data = report?.data;
  if (!Array.isArray(data) || data.length !== 1) {
    fail('coverage report must contain exactly one data entry');
  }
  const totals = data[0]?.totals;
  return {
    lines: validateMetric(totals?.lines, 'coverage.totals.lines'),
    regions: validateMetric(totals?.regions, 'coverage.totals.regions'),
    functions: validateMetric(totals?.functions, 'coverage.totals.functions'),
    instantiations: validateMetric(totals?.instantiations, 'coverage.totals.instantiations'),
  };
}

function evaluateSummary(reportPath, policy) {
  const report = readJson(reportPath, 'coverage report');
  const totals = coverageTotals(report);
  if (totals.lines.percent + Number.EPSILON < policy.minimumPercent) {
    fail(
      `line coverage ${totals.lines.percent.toFixed(4)}% is below required ` +
        `${policy.minimumPercent.toFixed(4)}%`,
    );
  }
  return totals;
}

function writePolicySummary(outputPath, policy, totals) {
  const summary = {
    schema_version: '1.0.0',
    engine: policy.raw.engine,
    scope: {
      packages: policy.packages,
      all_features: true,
      target_kinds: ['lib', 'tests'],
      locked: true,
      exclusions: policy.raw.scope.exclusions,
    },
    baseline: policy.raw.baseline,
    gate: {
      blocking_metric: 'lines',
      minimum_percent: policy.minimumPercent,
      observed_percent: totals.lines.percent,
      passed: true,
    },
    observed: totals,
  };
  mkdirSync(path.dirname(outputPath), { recursive: true });
  writeFileSync(outputPath, `${JSON.stringify(summary, null, 2)}\n`, 'utf8');
  return summary;
}

function run(executable, args) {
  const result = spawnSync(executable, args, {
    cwd: workspaceRoot,
    env: process.env,
    stdio: 'inherit',
    shell: false,
  });
  if (result.error) {
    fail(`could not run ${executable}: ${result.error.message}`);
  }
  if (result.status !== 0) {
    fail(`${executable} ${args.join(' ')} exited with ${result.status}`);
  }
}

function assertNonEmptyFile(filePath, label) {
  let size;
  try {
    size = statSync(filePath).size;
  } catch (error) {
    fail(`${label} is missing at ${filePath}: ${error.message}`);
  }
  if (size <= 0) {
    fail(`${label} is empty at ${filePath}`);
  }
}

function assertEngineVersion(policy) {
  const result = spawnSync('cargo', ['llvm-cov', '--version'], {
    cwd: workspaceRoot,
    encoding: 'utf8',
    shell: false,
  });
  if (result.error || result.status !== 0) {
    fail('cargo-llvm-cov is required and must be callable through cargo llvm-cov');
  }
  const expected = `cargo-llvm-cov ${policy.raw.engine.version}`;
  if (result.stdout.trim() !== expected) {
    fail(`expected ${expected}, observed ${result.stdout.trim() || '<empty>'}`);
  }
}

function resolveWorkspaceOutput(outputDir) {
  const resolved = path.resolve(workspaceRoot, outputDir);
  const relative = path.relative(workspaceRoot, resolved);
  if (relative === '' || relative.startsWith('..') || path.isAbsolute(relative)) {
    fail('coverage output directory must be a child of the workspace root');
  }
  return resolved;
}

function runCoverage(outputDir, policy) {
  assertEngineVersion(policy);
  const resolvedOutput = resolveWorkspaceOutput(outputDir);
  rmSync(resolvedOutput, { recursive: true, force: true });
  mkdirSync(resolvedOutput, { recursive: true });
  const packageArgs = policy.packages.flatMap((packageName) => ['-p', packageName]);
  const testArgs = [
    'llvm-cov',
    '--no-report',
    '--all-features',
    '--locked',
    '--lib',
    '--tests',
    ...packageArgs,
  ];

  run('cargo', ['llvm-cov', 'clean', '--workspace']);
  run('cargo', testArgs);

  const rawSummary = path.join(resolvedOutput, 'llvm-cov-summary.json');
  const lcovReport = path.join(resolvedOutput, 'lcov.info');
  const coberturaReport = path.join(resolvedOutput, 'cobertura.xml');
  run('cargo', [
    'llvm-cov',
    'report',
    '--json',
    '--summary-only',
    '--output-path',
    rawSummary,
  ]);
  run('cargo', ['llvm-cov', 'report', '--lcov', '--output-path', lcovReport]);
  run('cargo', [
    'llvm-cov',
    'report',
    '--cobertura',
    '--output-path',
    coberturaReport,
  ]);

  assertNonEmptyFile(rawSummary, 'LLVM coverage summary');
  assertNonEmptyFile(lcovReport, 'LCOV report');
  assertNonEmptyFile(coberturaReport, 'Cobertura report');
  const totals = evaluateSummary(rawSummary, policy);
  writePolicySummary(path.join(resolvedOutput, 'policy-summary.json'), policy, totals);
  console.log(
    `[coverage-gate] PASS lines=${totals.lines.covered}/${totals.lines.count} ` +
      `(${totals.lines.percent.toFixed(4)}%) minimum=${policy.minimumPercent.toFixed(4)}%`,
  );
}

function selfTest(policy) {
  const directory = mkdtempSync(path.join(tmpdir(), 'ranvier-coverage-gate-'));
  try {
    const malformed = path.join(directory, 'malformed.json');
    writeFileSync(malformed, '{not-json', 'utf8');
    let malformedRejected = false;
    try {
      evaluateSummary(malformed, policy);
    } catch {
      malformedRejected = true;
    }
    if (!malformedRejected) {
      fail('negative self-test accepted malformed JSON');
    }

    const low = path.join(directory, 'low.json');
    const count = 1000;
    const covered = Math.floor((policy.minimumPercent - 1) * 10);
    const metric = { count, covered, percent: (covered / count) * 100 };
    writeFileSync(
      low,
      `${JSON.stringify({
        data: [
          {
            totals: {
              lines: metric,
              regions: metric,
              functions: metric,
              instantiations: metric,
            },
          },
        ],
      })}\n`,
      'utf8',
    );
    let lowRejected = false;
    try {
      evaluateSummary(low, policy);
    } catch {
      lowRejected = true;
    }
    if (!lowRejected) {
      fail('negative self-test accepted below-threshold coverage');
    }

    for (const unsafeOutput of ['.', '..', path.join('..', 'outside')]) {
      let unsafeOutputRejected = false;
      try {
        resolveWorkspaceOutput(unsafeOutput);
      } catch {
        unsafeOutputRejected = true;
      }
      if (!unsafeOutputRejected) {
        fail(`negative self-test accepted unsafe output directory ${unsafeOutput}`);
      }
    }
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
  console.log('[coverage-gate] negative self-test passed');
}

function valueAfter(args, option, fallback) {
  const index = args.indexOf(option);
  if (index === -1) {
    return fallback;
  }
  if (index + 1 >= args.length) {
    fail(`${option} requires a value`);
  }
  const value = args[index + 1];
  if (value.startsWith('--')) {
    fail(`${option} requires a value`);
  }
  return value;
}

function main() {
  const policy = validatePolicy(readJson(policyPath, 'coverage policy'));
  const args = process.argv.slice(2);
  if (args.includes('--self-test')) {
    selfTest(policy);
    return;
  }
  if (args.includes('--run')) {
    runCoverage(valueAfter(args, '--output-dir', 'coverage'), policy);
    return;
  }
  if (args.includes('--check-summary')) {
    const reportPath = valueAfter(args, '--check-summary');
    const totals = evaluateSummary(path.resolve(process.cwd(), reportPath), policy);
    const outputPath = valueAfter(args, '--output', null);
    if (outputPath) {
      writePolicySummary(path.resolve(process.cwd(), outputPath), policy, totals);
    }
    console.log(
      `[coverage-gate] PASS lines=${totals.lines.covered}/${totals.lines.count} ` +
        `(${totals.lines.percent.toFixed(4)}%) minimum=${policy.minimumPercent.toFixed(4)}%`,
    );
    return;
  }
  fail('expected --run, --check-summary <path>, or --self-test');
}

try {
  main();
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error));
  process.exitCode = 1;
}
