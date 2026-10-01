import { packageCohortArgs } from './release_provenance_package_args.mjs';
import { createHash } from 'node:crypto';
import { copyFileSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { gunzipSync } from 'node:zlib';
import path from 'node:path';
import os from 'node:os';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const workspaceRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const policyPath = path.join(workspaceRoot, '.ranvier-supply-chain-policy.json');

function invariant(condition, message) {
  if (!condition) throw new Error(message);
}

function sha256(bytes) {
  return createHash('sha256').update(bytes).digest('hex');
}

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: workspaceRoot,
    encoding: 'utf8',
    maxBuffer: 128 * 1024 * 1024,
    windowsHide: true,
    ...options
  });
  if (result.error) throw result.error;
  invariant(result.status === 0, `${command} ${args.join(' ')} failed (${result.status}): ${(result.stderr || result.stdout).slice(-4000)}`);
  return { command: `${command} ${args.join(' ')}`, stdout: result.stdout ?? '', stderr: result.stderr ?? '' };
}

function normalizedRemote(value) {
  return value.trim().replace(/\.git$/, '').replace(/\/$/, '');
}

function validatePolicy(policy) {
  invariant(policy.schema_version === '1.0.0', 'unsupported policy schema');
  const crates = policy.provenance.publishable_crates;
  invariant(Array.isArray(crates) && crates.length === 12, 'policy must name exactly 12 publishable crates');
  invariant(new Set(crates).size === crates.length, 'policy publishable crates must be unique');
  invariant([...crates].sort().join('\n') === crates.join('\n'), 'policy publishable crates must be sorted');
}

function safeOutputPath(policy, requested, commit) {
  const allowedRoot = path.resolve(workspaceRoot, policy.provenance.output_root);
  mkdirSync(allowedRoot, { recursive: true });
  const stamp = new Date().toISOString().replace(/[-:]/g, '').replace(/\.\d{3}Z$/, 'Z');
  const output = path.resolve(workspaceRoot, requested ?? path.join(policy.provenance.output_root, `${stamp}-${commit.slice(0, 7)}`));
  const relative = path.relative(allowedRoot, output);
  invariant(relative !== '' && !relative.startsWith('..') && !path.isAbsolute(relative), 'output must be a child of the provenance output root');

  let cursor = allowedRoot;
  for (const segment of relative.split(path.sep).slice(0, -1)) {
    cursor = path.join(cursor, segment);
    if (existsSync(cursor)) invariant(!lstatSync(cursor).isSymbolicLink(), `output parent cannot be a symlink: ${cursor}`);
  }
  invariant(realpathSync(allowedRoot) === allowedRoot, 'provenance output root cannot be a symlink');
  if (existsSync(output)) {
    invariant(!lstatSync(output).isSymbolicLink(), 'provenance output directory cannot be a symlink');
    invariant(lstatSync(output).isDirectory(), 'provenance output path must be a directory');
    invariant(readdirSync(output).length === 0, 'output directory must not exist or must be empty');
  }
  return output;
}

function tarEntry(bytes, suffix) {
  for (let offset = 0; offset + 512 <= bytes.length;) {
    const header = bytes.subarray(offset, offset + 512);
    if (header.every((byte) => byte === 0)) break;
    const text = (start, length) => header.subarray(start, start + length).toString('utf8').replace(/\0.*$/, '');
    const name = [text(345, 155), text(0, 100)].filter(Boolean).join('/');
    const checksumText = text(148, 8).trim();
    invariant(/^[0-7]+$/.test(checksumText), 'invalid tar header checksum');
    const checksum = header.reduce((sum, byte, index) => sum + (index >= 148 && index < 156 ? 32 : byte), 0);
    invariant(checksum === Number.parseInt(checksumText, 8), 'tar header checksum mismatch');
    const sizeText = text(124, 12).trim();
    invariant(/^[0-7]+$/.test(sizeText), `invalid tar size for ${name}`);
    const size = Number.parseInt(sizeText, 8);
    invariant(Number.isSafeInteger(size) && size >= 0, `invalid tar size for ${name}`);
    const bodyStart = offset + 512;
    const next = bodyStart + Math.ceil(size / 512) * 512;
    invariant(Number.isSafeInteger(next) && next <= bytes.length, `truncated tar entry: ${name}`);
    if (name.endsWith(suffix)) {
      invariant(header[156] === 0 || header[156] === 48, `VCS evidence must be a regular file: ${name}`);
      return bytes.subarray(bodyStart, bodyStart + size);
    }
    offset = next;
  }
  throw new Error(`tar entry not found: *${suffix}`);
}

function embeddedVcsSha(crateBytes) {
  const tarBytes = gunzipSync(crateBytes);
  const vcs = JSON.parse(tarEntry(tarBytes, '/.cargo_vcs_info.json').toString('utf8'));
  invariant(typeof vcs.git?.sha1 === 'string' && /^[0-9a-f]{40}$/.test(vcs.git.sha1), '.cargo_vcs_info.json has invalid git.sha1');
  return vcs.git.sha1;
}

function assertVcsSha(observed, expected) {
  invariant(observed === expected, `embedded VCS SHA mismatch: ${observed} != ${expected}`);
}

function runSelfTest(policy) {
  validatePolicy(policy);
  const duplicate = structuredClone(policy);
  duplicate.provenance.publishable_crates[1] = duplicate.provenance.publishable_crates[0];
  let rejected = false;
  try { validatePolicy(duplicate); } catch { rejected = true; }
  invariant(rejected, 'duplicate publishable set passed');

  rejected = false;
  try { safeOutputPath(policy, '..\\outside-provenance', 'a'.repeat(40)); } catch { rejected = true; }
  invariant(rejected, 'unsafe output path passed');

  rejected = false;
  try { assertVcsSha('a'.repeat(40), 'b'.repeat(40)); } catch { rejected = true; }
  invariant(rejected, 'bad embedded VCS SHA passed');
  const valid = Buffer.alloc(1024);
  valid.write('fixture/.cargo_vcs_info.json', 0);
  valid.write('00000000002', 124);
  valid[156] = 48;
  valid.write('{}', 512);
  const seal = (b) => {
    b.fill(32, 148, 156);
    const checksum = b.subarray(0, 512).reduce((sum, byte) => sum + byte, 0);
    b.write(checksum.toString(8).padStart(6, '0') + '\0 ', 148);
    return b;
  };
  seal(valid);
  invariant(tarEntry(valid, '/.cargo_vcs_info.json').toString() === '{}', 'valid regular tar entry rejected');
  for (const [label, fixture] of [
    ['truncated', valid.subarray(0, 513)],
    ['checksum', (() => { const b = Buffer.from(valid); b[0] ^= 1; return b; })()],
    ['invalid-size', (() => { const b = Buffer.from(valid); b.write('0000000002x', 124); return seal(b); })()],
    ['link', (() => { const b = Buffer.from(valid); b[156] = 50; return seal(b); })()]
  ]) {
    rejected = false;
    try { tarEntry(fixture, '/.cargo_vcs_info.json'); } catch { rejected = true; }
    invariant(rejected, `malformed ${label} VCS evidence passed`);
    console.log(`[release-provenance] TAR_GUARD_PASS ${label}`);
  }
  console.log('[release-provenance] negative self-test passed');
}

function parseArgs(argv) {
  const args = { selfTest: false, output: null, verifyCrate: null, expectedCommit: null };
  const valueAt = (index) => {
    const value = argv[index];
    invariant(typeof value === 'string' && value.length > 0 && !value.startsWith('--'), 'option requires a nonempty value');
    return value;
  };
  for (let i = 0; i < argv.length; i += 1) {
    if (argv[i] === '--self-test') args.selfTest = true;
    else if (argv[i] === '--output') args.output = valueAt(++i);
    else if (argv[i] === '--verify-crate') args.verifyCrate = valueAt(++i);
    else if (argv[i] === '--expected-commit') args.expectedCommit = valueAt(++i);
    else throw new Error(`unknown argument: ${argv[i]}`);
  }
  invariant(Boolean(args.verifyCrate) === Boolean(args.expectedCommit), 'verify-crate and expected-commit must be supplied together');
  if (args.verifyCrate) {
    invariant(!args.output && !args.selfTest, 'crate verification cannot capture provenance or run self-test');
    invariant(/^[0-9a-f]{40}$/.test(args.expectedCommit), 'expected-commit must be40 lowercase hexadecimal digits');
  }
  return args;
}

const args = parseArgs(process.argv.slice(2));
const policyBytes = readFileSync(policyPath);
const policy = JSON.parse(policyBytes);
validatePolicy(policy);
if (args.verifyCrate) {
  const bytes = readFileSync(path.resolve(args.verifyCrate));
  assertVcsSha(embeddedVcsSha(bytes), args.expectedCommit);
  console.log(`[release-provenance] CRATE_VCS_PASS commit=${args.expectedCommit} sha256=${sha256(bytes)}`);
  process.exit(0);
}
if (args.selfTest) {
  runSelfTest(policy);
  if (!args.output) process.exit(0);
}

const commit = run('git', ['rev-parse', 'HEAD']).stdout.trim();
const remote = run('git', ['remote', 'get-url', 'origin']).stdout.trim();
invariant(normalizedRemote(remote) === normalizedRemote(policy.provenance.expected_remote), `unexpected origin remote: ${remote}`);
const statusBefore = run('git', ['status', '--porcelain=v1', '--untracked-files=all']).stdout.trim();
invariant(statusBefore === '', `source tree must be clean:\n${statusBefore}`);

const rustc = run('rustc', ['--version']).stdout.trim();
const cargoVersion = run('cargo', ['--version']).stdout.trim();
invariant(rustc.startsWith(`rustc ${policy.tools.rust} `), `rustc must be exactly ${policy.tools.rust}: ${rustc}`);
invariant(cargoVersion.startsWith(`cargo ${policy.tools.cargo} `), `cargo must be exactly ${policy.tools.cargo}: ${cargoVersion}`);
invariant(Number(process.versions.node.split('.')[0]) === policy.tools.node_major, `Node must be major ${policy.tools.node_major}`);

const metadata = JSON.parse(run('cargo', ['metadata', '--format-version', '1', '--no-deps', '--locked']).stdout);
const allFeatureMetadata = JSON.parse(
  run('cargo', ['metadata', '--format-version', '1', '--all-features', '--locked']).stdout
);
const originalExternalPackages = new Set(
  allFeatureMetadata.packages
    .filter((pkg) => pkg.source !== null)
    .map((pkg) => `${pkg.name}|${pkg.version}|${pkg.source}`)
);
const workspaceIds = new Set(metadata.workspace_members);
const publishable = metadata.packages
  .filter((pkg) => workspaceIds.has(pkg.id) && !(Array.isArray(pkg.publish) && pkg.publish.length === 0))
  .sort((a, b) => a.name.localeCompare(b.name));
invariant(publishable.map((pkg) => pkg.name).join('\n') === policy.provenance.publishable_crates.join('\n'), 'derived publishable crate set differs from policy');

const output = safeOutputPath(policy, args.output, commit);
mkdirSync(output, { recursive: true });
const commands = [];
const artifacts = [];
// Package the selected cohort together so Cargo resolves unpublished workspace
// dependencies against this cohort rather than older same-version registry crates.
const packageArgs = packageCohortArgs(publishable.map(pkg => pkg.name));
const packageCommand = run('cargo', packageArgs);
commands.push(packageCommand.command);
for (const pkg of publishable) {
  const source = path.join(metadata.target_directory, 'package', `${pkg.name}-${pkg.version}.crate`);
  invariant(existsSync(source), `cargo package artifact missing: ${source}`);
  const destination = path.join(output, path.basename(source));
  copyFileSync(source, destination);
  const bytes = readFileSync(destination);
  assertVcsSha(embeddedVcsSha(bytes), commit);
  artifacts.push({
    kind: 'crate',
    crate: pkg.name,
    version: pkg.version,
    file: path.basename(destination),
    bytes: bytes.length,
    sha256: sha256(bytes),
    embedded_vcs_sha: commit
  });
}

const verificationRoot = mkdtempSync(path.join(metadata.target_directory, 'release-cohort-'));
let cohortVerification;
try {
  const members = [];
  for (const pkg of publishable) {
    const member = `${pkg.name}-${pkg.version}`;
    members.push(member);
    const artifact = path.join(output, `${member}.crate`);
    const extraction = run('tar', ['-xzf', artifact, '-C', verificationRoot]);
    commands.push(extraction.command);
  }

  const quotedMembers = members.map((member) => `  "${member}",`).join('\n');
  const patches = publishable
    .map((pkg) => `${JSON.stringify(pkg.name)} = { path = ${JSON.stringify(`${pkg.name}-${pkg.version}`)} }`)
    .join('\n');
  const cohortManifest = `[workspace]\nresolver = "2"\nmembers = [\n${quotedMembers}\n]\n\n[patch.crates-io]\n${patches}\n`;
  const cohortManifestPath = path.join(verificationRoot, 'Cargo.toml');
  writeFileSync(cohortManifestPath, cohortManifest, 'utf8');
  copyFileSync(path.join(workspaceRoot, 'Cargo.lock'), path.join(verificationRoot, 'Cargo.lock'));

  const resolution = run(
    'cargo',
    ['metadata', '--format-version', '1', '--all-features', '--offline'],
    { cwd: verificationRoot }
  );
  commands.push(resolution.command);
  const cohortMetadata = JSON.parse(resolution.stdout);
  const cohortExternalPackages = cohortMetadata.packages
    .filter((pkg) => pkg.source !== null)
    .map((pkg) => `${pkg.name}|${pkg.version}|${pkg.source}`);
  const externalDrift = cohortExternalPackages.filter((pkg) => !originalExternalPackages.has(pkg));
  invariant(
    externalDrift.length === 0,
    `cohort resolution introduced external dependency drift:\n${externalDrift.join('\n')}`
  );
  const cohortInternalPackages = cohortMetadata.packages
    .filter((pkg) => pkg.source === null)
    .map((pkg) => pkg.name)
    .sort();
  invariant(
    cohortInternalPackages.join('\n') === policy.provenance.publishable_crates.join('\n'),
    'cohort internal package set differs from the 12 publishable artifacts'
  );

  const verification = run(
    'cargo',
    ['check', '--workspace', '--all-features', '--locked', '--offline'],
    { cwd: verificationRoot }
  );
  commands.push(verification.command);
  cohortVerification = {
    mode: 'extracted-package-cohort',
    publishable_crates: publishable.length,
    external_dependencies: 'offline lock derived from workspace Cargo.lock; no new name/version/source tuple',
    external_dependency_tuples: cohortExternalPackages.length,
    internal_dependencies: '12 extracted .crate artifacts via local crates.io patch',
    command: verification.command,
    manifest_sha256: sha256(Buffer.from(cohortManifest)),
    derived_lock_sha256: sha256(readFileSync(path.join(verificationRoot, 'Cargo.lock')))
  };
} finally {
  rmSync(verificationRoot, { recursive: true, force: true });
}

const sourceArchive = `ranvier-source-${commit}.tar.gz`;
const sourceArchivePath = path.join(output, sourceArchive);
const archiveCommand = run('git', ['archive', '--format=tar.gz', '--output', sourceArchivePath, 'HEAD']);
commands.push(archiveCommand.command);
const sourceBytes = readFileSync(sourceArchivePath);
artifacts.push({ kind: 'source', file: sourceArchive, bytes: sourceBytes.length, sha256: sha256(sourceBytes), commit });

const statusAfter = run('git', ['status', '--porcelain=v1', '--untracked-files=all']).stdout.trim();
invariant(statusAfter === '', `packaging changed source tree:\n${statusAfter}`);

artifacts.sort((a, b) => a.file.localeCompare(b.file));
const checksums = artifacts.map((artifact) => `${artifact.sha256}  ${artifact.file}`).join('\n') + '\n';
writeFileSync(path.join(output, 'SHA256SUMS'), checksums, 'utf8');
const provenance = {
  schema_version: '1.0.0',
  generated_at: new Date().toISOString(),
  claim_level: 'unsigned-local-build-and-source-provenance',
  attestation_required_for_signed_identity: policy.tools.attestation_action,
  source: {
    repository: normalizedRemote(remote),
    commit,
    tree_clean_before: true,
    tree_clean_after: true,
    cargo_lock_sha256: sha256(readFileSync(path.join(workspaceRoot, 'Cargo.lock'))),
    workspace_manifest_sha256: sha256(readFileSync(path.join(workspaceRoot, 'Cargo.toml'))),
    policy_sha256: sha256(policyBytes)
  },
  environment: {
    rustc,
    cargo: cargoVersion,
    node: process.version,
    os: os.platform(),
    architecture: os.arch(),
    release: os.release()
  },
  commands,
  cohort_verification: cohortVerification,
  artifacts,
  checksum_manifest: { file: 'SHA256SUMS', sha256: sha256(Buffer.from(checksums)) },
  non_claims: [
    'This local provenance is not a cryptographic signature.',
    'Artifact hashes do not prove that source or dependencies are vulnerability-free.',
    'Published registry bytes require post-publication checksum comparison.'
  ]
};
writeFileSync(path.join(output, 'provenance.json'), `${JSON.stringify(provenance, null, 2)}\n`, 'utf8');
console.log(`[release-provenance] passed: ${artifacts.length - 1} crates plus source archive`);
console.log(`[release-provenance] output: ${path.relative(workspaceRoot, output).replaceAll('\\', '/')}`);
