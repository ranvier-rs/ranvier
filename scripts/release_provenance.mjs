import { createHash } from 'node:crypto';
import { copyFileSync, existsSync, lstatSync, mkdirSync, readFileSync, readdirSync, realpathSync, writeFileSync } from 'node:fs';
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
    const sizeText = text(124, 12).trim();
    const size = Number.parseInt(sizeText || '0', 8);
    invariant(Number.isFinite(size), `invalid tar size for ${name}`);
    const bodyStart = offset + 512;
    if (name.endsWith(suffix)) return bytes.subarray(bodyStart, bodyStart + size);
    offset = bodyStart + Math.ceil(size / 512) * 512;
  }
  throw new Error(`tar entry not found: *${suffix}`);
}

function embeddedVcsSha(crateBytes) {
  const tarBytes = gunzipSync(crateBytes);
  const vcs = JSON.parse(tarEntry(tarBytes, '/.cargo_vcs_info.json').toString('utf8'));
  invariant(typeof vcs.git?.sha1 === 'string', '.cargo_vcs_info.json has no git.sha1');
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
  console.log('[release-provenance] negative self-test passed');
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
invariant(Number(process.versions.node.split('.')[0]) === policy.tools.node_major, `Node must be major ${policy.tools.node_major}`);

const metadata = JSON.parse(run('cargo', ['metadata', '--format-version', '1', '--no-deps', '--locked']).stdout);
const workspaceIds = new Set(metadata.workspace_members);
const publishable = metadata.packages
  .filter((pkg) => workspaceIds.has(pkg.id) && !(Array.isArray(pkg.publish) && pkg.publish.length === 0))
  .sort((a, b) => a.name.localeCompare(b.name));
invariant(publishable.map((pkg) => pkg.name).join('\n') === policy.provenance.publishable_crates.join('\n'), 'derived publishable crate set differs from policy');

const output = safeOutputPath(policy, args.output, commit);
mkdirSync(output, { recursive: true });
const commands = [];
const artifacts = [];
for (const pkg of publishable) {
  const command = run('cargo', ['package', '--locked', '-p', pkg.name]);
  commands.push(command.command);
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
