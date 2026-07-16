import { createHash } from 'node:crypto';
import { existsSync, lstatSync, mkdirSync, readFileSync, realpathSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const workspaceRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const policyPath = path.join(workspaceRoot, '.ranvier-supply-chain-policy.json');
const lockPath = path.join(workspaceRoot, 'Cargo.lock');
const allowedOutputRoot = path.join(workspaceRoot, 'target', 'supply-chain');

function invariant(condition, message) {
  if (!condition) throw new Error(message);
}

function sha256(bytes) {
  return createHash('sha256').update(bytes).digest('hex');
}

function dateOnly(value, field) {
  invariant(/^\d{4}-\d{2}-\d{2}$/.test(value ?? ''), `${field} must use YYYY-MM-DD`);
  const parsed = new Date(`${value}T00:00:00Z`);
  invariant(Number.isFinite(parsed.getTime()) && parsed.toISOString().slice(0, 10) === value, `${field} is invalid`);
  return parsed;
}

function daysBetween(from, to) {
  return (to - from) / 86_400_000;
}

function roundUpOneDecimal(value) {
  return Math.ceil((value * 10) - 1e-6) / 10;
}

function cvss3BaseScore(vector) {
  if (typeof vector === 'number' && Number.isFinite(vector)) return vector;
  if (typeof vector !== 'string') return null;
  if (/^\d+(?:\.\d+)?$/.test(vector)) return Number(vector);

  const parts = vector.split('/');
  if (!/^CVSS:3\.[01]$/.test(parts.shift() ?? '')) return null;
  const metrics = Object.fromEntries(parts.map((part) => part.split(':')));
  const required = ['AV', 'AC', 'PR', 'UI', 'S', 'C', 'I', 'A'];
  if (required.some((metric) => typeof metrics[metric] !== 'string')) return null;

  const weights = {
    AV: { N: 0.85, A: 0.62, L: 0.55, P: 0.2 },
    AC: { L: 0.77, H: 0.44 },
    UI: { N: 0.85, R: 0.62 },
    CIA: { H: 0.56, L: 0.22, N: 0 }
  };
  const scopeChanged = metrics.S === 'C';
  const privilege = scopeChanged
    ? { N: 0.85, L: 0.68, H: 0.5 }
    : { N: 0.85, L: 0.62, H: 0.27 };
  const av = weights.AV[metrics.AV];
  const ac = weights.AC[metrics.AC];
  const pr = privilege[metrics.PR];
  const ui = weights.UI[metrics.UI];
  const confidentiality = weights.CIA[metrics.C];
  const integrity = weights.CIA[metrics.I];
  const availability = weights.CIA[metrics.A];
  if ([av, ac, pr, ui, confidentiality, integrity, availability].some((value) => value === undefined)) return null;

  const impactSubscore = 1 - ((1 - confidentiality) * (1 - integrity) * (1 - availability));
  const impact = scopeChanged
    ? (7.52 * (impactSubscore - 0.029)) - (3.25 * ((impactSubscore - 0.02) ** 15))
    : 6.42 * impactSubscore;
  if (impact <= 0) return 0;
  const exploitability = 8.22 * av * ac * pr * ui;
  const base = scopeChanged
    ? Math.min(1.08 * (impact + exploitability), 10)
    : Math.min(impact + exploitability, 10);
  return roundUpOneDecimal(base);
}

function normalizeSeverity(finding) {
  const explicit = String(finding.advisory?.severity ?? '').toLowerCase();
  if (['critical', 'high', 'medium', 'low'].includes(explicit)) return explicit;
  const score = cvss3BaseScore(finding.advisory?.cvss);
  if (score !== null) {
    if (score >= 9) return 'critical';
    if (score >= 7) return 'high';
    if (score >= 4) return 'medium';
    if (score > 0) return 'low';
  }
  return 'unknown';
}

function findingKey(finding) {
  return `${finding.advisory?.id}|${finding.package?.name}|${finding.package?.version}`;
}

function recordKey(record) {
  return `${record.advisory_id}|${record.package?.name}|${record.package?.version}`;
}

function validatePolicy(policy) {
  invariant(policy.schema_version === '1.0.0', 'unsupported policy schema');
  invariant(policy.ownership.primary !== policy.ownership.backup, 'primary and backup must be distinct');
  invariant(policy.tools.node_major === 24, 'supply-chain Node major must remain 24');
  invariant(policy.advisory.max_review_days > 0, 'max_review_days must be positive');
  invariant(policy.advisory.max_expiry_days >= policy.advisory.max_review_days, 'max_expiry_days must cover review window');
}

function validateRecord(policy, record, now) {
  const errors = [];
  const add = (condition, message) => {
    if (!condition) errors.push(message);
  };
  try {
    add(/^RUSTSEC-\d{4}-\d{4}$/.test(record.advisory_id ?? ''), 'advisory_id must be a RustSec ID');
    add(typeof record.package?.name === 'string' && record.package.name.length > 0, 'package.name is required');
    add(typeof record.package?.version === 'string' && record.package.version.length > 0, 'package.version is required');
    add(record.decision === 'temporary_allow', 'decision must be temporary_allow');
    add(policy.advisory.temporary_allow_severities.includes(record.severity), 'severity cannot be temporarily allowed');
    add(record.owner === policy.ownership.primary, 'record owner must be the primary');
    add(record.backup === policy.ownership.backup, 'record backup must match policy');
    add(record.owner !== record.backup, 'record owners must be distinct');
    add(record.approvals?.primary === policy.ownership.primary, 'primary approval is required');
    add(record.approvals?.backup === policy.ownership.backup, 'backup approval is required');
    for (const field of ['affected_features', 'rationale', 'mitigation', 'upgrade_path', 'evidence']) {
      add(typeof record[field] === 'string' && record[field].length >= 12, `${field} must be substantive`);
    }
    const detected = dateOnly(record.detected_on, 'detected_on');
    const approved = dateOnly(record.approved_on, 'approved_on');
    const review = dateOnly(record.review_by, 'review_by');
    const expires = dateOnly(record.expires_on, 'expires_on');
    add(approved >= detected, 'approved_on precedes detected_on');
    add(review >= approved, 'review_by precedes approved_on');
    add(expires >= review, 'expires_on precedes review_by');
    add(daysBetween(approved, review) <= policy.advisory.max_review_days, 'review window exceeds policy');
    add(daysBetween(approved, expires) <= policy.advisory.max_expiry_days, 'expiry window exceeds policy');
    add(now <= expires, 'triage record is expired');
  } catch (error) {
    errors.push(error.message);
  }
  return errors;
}

export function evaluateAdvisories(policy, registry, findings, now = new Date()) {
  validatePolicy(policy);
  invariant(registry.schema_version === '1.0.0', 'unsupported triage registry schema');
  invariant(Array.isArray(registry.advisories), 'triage advisories must be an array');
  const records = new Map();
  const errors = [];
  for (const record of registry.advisories) {
    const key = recordKey(record);
    if (records.has(key)) errors.push(`duplicate triage record: ${key}`);
    records.set(key, record);
    for (const error of validateRecord(policy, record, now)) errors.push(`${key}: ${error}`);
  }

  const matched = new Set();
  const normalizedFindings = findings.map((finding) => ({
    key: findingKey(finding),
    advisory_id: finding.advisory?.id,
    package: finding.package,
    severity: normalizeSeverity(finding),
    title: finding.advisory?.title,
    url: finding.advisory?.url
  }));

  for (const finding of normalizedFindings) {
    const record = records.get(finding.key);
    if (!record) {
      errors.push(`untriaged vulnerability: ${finding.key} severity=${finding.severity}`);
      continue;
    }
    matched.add(finding.key);
    if (policy.advisory.release_blocking_severities.includes(finding.severity)) {
      errors.push(`release-blocking ${finding.severity} vulnerability: ${finding.key}`);
    }
    if (finding.severity !== 'unknown' && finding.severity !== record.severity) {
      errors.push(`severity mismatch for ${finding.key}: scanner=${finding.severity} record=${record.severity}`);
    }
  }

  for (const key of records.keys()) {
    if (!matched.has(key)) errors.push(`stale triage record no longer present in scan: ${key}`);
  }

  return { passed: errors.length === 0, errors, findings: normalizedFindings };
}

function run(command, args) {
  const result = spawnSync(command, args, {
    cwd: workspaceRoot,
    encoding: 'utf8',
    maxBuffer: 64 * 1024 * 1024,
    windowsHide: true
  });
  if (result.error) throw result.error;
  return {
    command: `${command} ${args.join(' ')}`,
    status: result.status,
    stdout: result.stdout ?? '',
    stderr: result.stderr ?? ''
  };
}

function exactToolVersion(command, args, expected) {
  const result = run(command, args);
  invariant(result.status === 0, `${command} version command failed`);
  const output = `${result.stdout}\n${result.stderr}`;
  invariant(new RegExp(`(?:^|\\s)v?${expected.replaceAll('.', '\\.')}\\b`).test(output), `${command} must be exactly ${expected}; found ${output.trim()}`);
  return output.trim();
}

function parseDenyDiagnostics(result) {
  const diagnostics = [];
  for (const line of `${result.stdout}\n${result.stderr}`.split(/\r?\n/)) {
    if (!line.trim().startsWith('{')) continue;
    try {
      const value = JSON.parse(line);
      if (value.type === 'diagnostic') diagnostics.push(value.fields);
    } catch {
      // cargo-deny may interleave non-JSON progress text; only JSON-looking
      // malformed lines are rejected below when no usable summary exists.
    }
  }
  const summaryLines = `${result.stdout}\n${result.stderr}`
    .split(/\r?\n/)
    .filter((line) => line.trim().startsWith('{'))
    .map((line) => {
      try { return JSON.parse(line); } catch { return null; }
    })
    .filter((value) => value?.type === 'summary');
  invariant(summaryLines.length === 1, 'cargo-deny advisory output must contain exactly one JSON summary');
  return { diagnostics, summary: summaryLines[0].fields };
}

function findingFromDenyDiagnostic(fields) {
  const advisory = fields.advisory;
  const crateNode = fields.graphs?.find((graph) => graph.Krate)?.Krate;
  invariant(advisory?.id && crateNode?.name && crateNode?.version, 'cargo-deny advisory diagnostic is incomplete');
  return {
    advisory: {
      id: advisory.id,
      title: advisory.title,
      cvss: advisory.cvss,
      severity: advisory.severity ?? null,
      url: advisory.url,
      informational: advisory.informational
    },
    package: {
      name: crateNode.name,
      version: crateNode.version
    }
  };
}

function safeOutputPath(requested) {
  const output = path.resolve(workspaceRoot, requested ?? 'target/supply-chain/supply-chain-gate.json');
  const relative = path.relative(allowedOutputRoot, output);
  invariant(relative !== '' && !relative.startsWith('..') && !path.isAbsolute(relative), 'output must be a file under target/supply-chain');
  mkdirSync(allowedOutputRoot, { recursive: true });
  invariant(realpathSync(allowedOutputRoot) === allowedOutputRoot, 'supply-chain output root cannot be a symlink');
  let cursor = allowedOutputRoot;
  for (const segment of relative.split(path.sep).slice(0, -1)) {
    cursor = path.join(cursor, segment);
    if (existsSync(cursor)) invariant(!lstatSync(cursor).isSymbolicLink(), `output parent cannot be a symlink: ${cursor}`);
  }
  if (existsSync(output)) invariant(!lstatSync(output).isSymbolicLink(), 'output file cannot be a symlink');
  return output;
}

function runSelfTest(policy) {
  const now = new Date('2026-07-17T00:00:00Z');
  const finding = {
    advisory: { id: 'RUSTSEC-2099-0001', title: 'synthetic', severity: 'low' },
    package: { name: 'synthetic-dependency', version: '1.0.0' }
  };
  invariant(evaluateAdvisories(policy, { schema_version: '1.0.0', advisories: [] }, [], now).passed, 'empty clean scan failed');
  invariant(!evaluateAdvisories(policy, { schema_version: '1.0.0', advisories: [] }, [finding], now).passed, 'untriaged finding passed');

  const record = {
    advisory_id: 'RUSTSEC-2099-0001',
    package: { name: 'synthetic-dependency', version: '1.0.0' },
    severity: 'low',
    decision: 'temporary_allow',
    owner: policy.ownership.primary,
    backup: policy.ownership.backup,
    approvals: { primary: policy.ownership.primary, backup: policy.ownership.backup },
    affected_features: 'Feature is not enabled in release artifacts.',
    rationale: 'Temporary exception for the negative self-test fixture.',
    mitigation: 'The affected path remains disabled and monitored.',
    upgrade_path: 'Upgrade immediately when a fixed dependency is available.',
    evidence: 'security/triage/synthetic-evidence.md',
    detected_on: '2026-07-16',
    approved_on: '2026-07-16',
    review_by: '2026-07-30',
    expires_on: '2026-08-15'
  };
  const registry = { schema_version: '1.0.0', advisories: [record] };
  invariant(evaluateAdvisories(policy, registry, [finding], now).passed, 'valid low triage did not pass');

  const highFinding = structuredClone(finding);
  highFinding.advisory.severity = 'high';
  const highRecord = structuredClone(record);
  highRecord.severity = 'high';
  invariant(!evaluateAdvisories(policy, { schema_version: '1.0.0', advisories: [highRecord] }, [highFinding], now).passed, 'triaged High finding passed');

  const expired = structuredClone(record);
  expired.expires_on = '2026-07-16';
  invariant(!evaluateAdvisories(policy, { schema_version: '1.0.0', advisories: [expired] }, [finding], now).passed, 'expired triage passed');
  invariant(!evaluateAdvisories(policy, registry, [], now).passed, 'stale triage passed');

  invariant(normalizeSeverity({ advisory: { cvss: 'CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H' } }) === 'critical', 'critical CVSS vector was misclassified');
  invariant(normalizeSeverity({ advisory: { cvss: 'CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:N/A:N' } }) === 'high', 'high CVSS vector was misclassified');
  invariant(normalizeSeverity({ advisory: { cvss: 'CVSS:3.1/AV:N/AC:H/PR:N/UI:N/S:U/C:H/I:N/A:N' } }) === 'medium', 'medium CVSS vector was misclassified');
  invariant(normalizeSeverity({ advisory: { cvss: 'CVSS:3.1/AV:P/AC:H/PR:H/UI:R/S:U/C:L/I:N/A:N' } }) === 'low', 'low CVSS vector was misclassified');
  let invalidDateRejected = false;
  try { dateOnly('2026-02-30', 'synthetic_date'); } catch { invalidDateRejected = true; }
  invariant(invalidDateRejected, 'calendar-invalid date passed');

  const sameOwner = structuredClone(policy);
  sameOwner.ownership.backup = sameOwner.ownership.primary;
  let rejected = false;
  try { evaluateAdvisories(sameOwner, { schema_version: '1.0.0', advisories: [] }, [], now); } catch { rejected = true; }
  invariant(rejected, 'same-owner policy passed');
  console.log('[supply-chain] negative self-test passed');
}

function parseArgs(argv) {
  const args = { selfTest: false, output: null };
  for (let i = 0; i < argv.length; i += 1) {
    if (argv[i] === '--self-test') args.selfTest = true;
    else if (argv[i] === '--output') args.output = argv[++i];
    else throw new Error(`unknown argument: ${argv[i]}`);
  }
  return args;
}

const args = parseArgs(process.argv.slice(2));
const policyBytes = readFileSync(policyPath);
const policy = JSON.parse(policyBytes);
validatePolicy(policy);
if (args.selfTest) runSelfTest(policy);

const outputPath = safeOutputPath(args.output);
const triagePath = path.resolve(workspaceRoot, policy.advisory.triage_registry);
const registryBytes = readFileSync(triagePath);
const registry = JSON.parse(registryBytes);
const evidence = {
  schema_version: '1.0.0',
  evaluated_at: new Date().toISOString(),
  policy_sha256: sha256(policyBytes),
  triage_registry_sha256: sha256(registryBytes),
  cargo_lock_sha256: sha256(readFileSync(lockPath)),
  tools: {},
  advisory: null,
  license_source: null,
  passed: false,
  errors: []
};

try {
  evidence.tools.cargo_audit = exactToolVersion('cargo', ['audit', '--version'], policy.tools.cargo_audit);
  evidence.tools.cargo_deny = exactToolVersion('cargo', ['deny', '--version'], policy.tools.cargo_deny);
  evidence.tools.node = process.version;
  invariant(Number(process.versions.node.split('.')[0]) === policy.tools.node_major, `Node must be major ${policy.tools.node_major}`);

  const audit = run('cargo', ['audit', '--json', '--file', 'Cargo.lock']);
  let auditJson;
  try {
    auditJson = JSON.parse(audit.stdout);
  } catch (error) {
    throw new Error(`cargo audit did not return valid JSON: ${error.message}; stderr=${audit.stderr.slice(-2000)}`);
  }
  const lockfileFindings = auditJson.vulnerabilities?.list ?? [];
  const denyAdvisories = run('cargo', ['deny', '--format', 'json', '--all-features', '--exclude-unpublished', 'check', 'advisories']);
  const parsedDeny = parseDenyDiagnostics(denyAdvisories);
  const vulnerabilityDiagnostics = parsedDeny.diagnostics.filter((diagnostic) => diagnostic.advisory && diagnostic.advisory.informational == null);
  const informationalDiagnostics = parsedDeny.diagnostics.filter((diagnostic) => diagnostic.advisory?.informational != null);
  const scopedFindings = vulnerabilityDiagnostics.map(findingFromDenyDiagnostic);
  const advisory = evaluateAdvisories(policy, registry, scopedFindings);
  const scopedKeys = new Set(advisory.findings.map((finding) => finding.key));
  const broadInventory = lockfileFindings.map((finding) => ({
    key: findingKey(finding),
    advisory_id: finding.advisory?.id,
    package: finding.package,
    severity: normalizeSeverity(finding),
    title: finding.advisory?.title,
    url: finding.advisory?.url,
    in_release_scope: scopedKeys.has(findingKey(finding))
  }));
  const informationalErrors = informationalDiagnostics
    .filter((diagnostic) => diagnostic.severity === 'error')
    .map((diagnostic) => `${diagnostic.code} advisory is release-blocking: ${diagnostic.advisory.id}|${diagnostic.advisory.package}`);
  evidence.advisory = {
    lockfile_inventory: {
      command: audit.command,
      exit_code: audit.status,
      database: auditJson.database ?? null,
      lockfile: auditJson.lockfile ?? null,
      warnings: auditJson.warnings ?? {},
      findings: broadInventory
    },
    release_scope: {
      command: denyAdvisories.command,
      exit_code: denyAdvisories.status,
      authority: policy.advisory.release_scope_authority,
      cargo_deny_summary: parsedDeny.summary,
      informational: informationalDiagnostics.map((diagnostic) => ({
        id: diagnostic.advisory.id,
        package: diagnostic.advisory.package,
        kind: diagnostic.advisory.informational,
        severity: diagnostic.severity
      })),
      ...advisory
    }
  };
  evidence.errors.push(...advisory.errors);
  evidence.errors.push(...informationalErrors);
  if (denyAdvisories.status !== 0 && parsedDeny.diagnostics.length === 0) {
    evidence.errors.push(`cargo-deny advisory gate failed without a parseable finding (exit ${denyAdvisories.status})`);
  }

  const deny = run('cargo', ['deny', '--all-features', '--exclude-unpublished', 'check', 'licenses', 'sources']);
  evidence.license_source = {
    command: deny.command,
    exit_code: deny.status,
    stdout_tail: deny.stdout.split(/\r?\n/).slice(-80),
    stderr_tail: deny.stderr.split(/\r?\n/).slice(-80),
    passed: deny.status === 0
  };
  if (deny.status !== 0) evidence.errors.push(`cargo-deny license/source gate failed with exit ${deny.status}`);
  evidence.passed = evidence.errors.length === 0;
} catch (error) {
  evidence.errors.push(error.message);
  evidence.passed = false;
}

mkdirSync(path.dirname(outputPath), { recursive: true });
writeFileSync(outputPath, `${JSON.stringify(evidence, null, 2)}\n`, 'utf8');
console.log(`[supply-chain] evidence: ${path.relative(workspaceRoot, outputPath).replaceAll('\\', '/')}`);
if (!evidence.passed) {
  console.error(`[supply-chain] FAILED\n${evidence.errors.map((error) => `- ${error}`).join('\n')}`);
  process.exitCode = 1;
} else {
  console.log(`[supply-chain] passed: ${evidence.advisory.release_scope.findings.length} in-scope vulnerabilities, license/source policy clean`);
}
