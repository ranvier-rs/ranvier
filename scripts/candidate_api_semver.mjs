#!/usr/bin/env node
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const workspaceRoot = path.resolve(scriptDir, '..');
const baselinePath = path.join(workspaceRoot, 'api-stable-candidate-baseline.json');

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: workspaceRoot,
    encoding: 'utf8',
    stdio: options.inherit ? 'inherit' : 'pipe'
  });
  if ((result.status ?? 1) !== 0) {
    const detail = result.stderr?.trim() || result.stdout?.trim() || `exit ${result.status}`;
    throw new Error(`${command} ${args.join(' ')} failed: ${detail}`);
  }
  return result.stdout?.trim() ?? '';
}

function selectedPackages(allowed) {
  const selected = [];
  for (let index = 2; index < process.argv.length; index += 1) {
    if (process.argv[index] !== '--package' || !process.argv[index + 1]) {
      throw new Error('usage: node scripts/candidate_api_semver.mjs [--package <candidate-package>]...');
    }
    selected.push(process.argv[index + 1]);
    index += 1;
  }
  const result = selected.length > 0 ? selected : allowed;
  for (const packageName of result) {
    if (!allowed.includes(packageName)) {
      throw new Error(`${packageName} is not a candidate-bearing package in the frozen baseline`);
    }
  }
  return [...new Set(result)];
}

try {
  run(process.execPath, ['scripts/candidate_api_baseline.mjs', '--check'], { inherit: true });
  const baseline = JSON.parse(readFileSync(baselinePath, 'utf8'));
  const resolved = run('git', ['rev-parse', `${baseline.baseline_ref}^{commit}`]);
  if (resolved !== baseline.baseline_commit) {
    throw new Error(`${baseline.baseline_ref} resolved to ${resolved}, expected ${baseline.baseline_commit}`);
  }
  const version = run('cargo', ['semver-checks', '--version']);
  const match = version.match(/(\d+)\.(\d+)\.(\d+)/);
  if (!match || Number(match[1]) === 0 && Number(match[2]) < 48) {
    throw new Error(`cargo-semver-checks 0.48.0 or newer is required; found ${version}`);
  }

  const packages = selectedPackages(baseline.cargo_semver_packages);
  console.log(`candidate SemVer baseline: ${baseline.baseline_ref} (${baseline.baseline_commit})`);
  console.log(`candidate packages: ${packages.join(', ')}`);
  for (const packageName of packages) {
    console.log(`\n[semver] ${packageName}`);
    run('cargo', [
      'semver-checks',
      'check-release',
      '-p', packageName,
      '--baseline-rev', baseline.baseline_ref,
      '--all-features',
      '--color', 'never'
    ], { inherit: true });
  }
  console.log(`\ncandidate SemVer gate: pass (${packages.length} packages)`);
} catch (error) {
  console.error(`candidate SemVer gate: FAILED\n${error.message}`);
  process.exit(1);
}
