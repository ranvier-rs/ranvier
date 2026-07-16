import { readFileSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const workspaceRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const policy = JSON.parse(readFileSync(path.join(workspaceRoot, '.ranvier-supply-chain-policy.json'), 'utf8'));
invariant(Array.isArray(policy.provenance?.publishable_crates), 'policy publishable crate list is required');
invariant(policy.provenance.publishable_crates.length === 12, 'policy must name exactly 12 publishable crates');
invariant(new Set(policy.provenance.publishable_crates).size === 12, 'policy publishable crates must be unique');
const expected = new Set(policy.provenance.publishable_crates);

function invariant(condition, message) {
  if (!condition) throw new Error(message);
}

function metadata() {
  const result = spawnSync('cargo', ['metadata', '--format-version', '1', '--no-deps', '--locked'], {
    cwd: workspaceRoot,
    encoding: 'utf8',
    maxBuffer: 64 * 1024 * 1024,
    windowsHide: true
  });
  invariant(result.status === 0, `cargo metadata failed: ${result.stderr}`);
  return JSON.parse(result.stdout);
}

function boundaryViolations(cargoMetadata) {
  const workspaceIds = new Set(cargoMetadata.workspace_members);
  return cargoMetadata.packages
    .filter((pkg) => workspaceIds.has(pkg.id))
    .filter((pkg) => {
      const explicitlyPrivate = Array.isArray(pkg.publish) && pkg.publish.length === 0;
      return expected.has(pkg.name) ? explicitlyPrivate : !explicitlyPrivate;
    })
    .sort((a, b) => a.name.localeCompare(b.name));
}

function markPrivate(manifestPath) {
  const original = readFileSync(manifestPath, 'utf8');
  const newline = original.includes('\r\n') ? '\r\n' : '\n';
  const lines = original.split(/\r?\n/);
  const packageStart = lines.findIndex((line) => line.trim() === '[package]');
  invariant(packageStart >= 0, `manifest has no [package]: ${manifestPath}`);
  let packageEnd = lines.findIndex((line, index) => index > packageStart && /^\s*\[/.test(line));
  if (packageEnd < 0) packageEnd = lines.length;
  invariant(!lines.slice(packageStart + 1, packageEnd).some((line) => /^\s*publish\s*=/.test(line)), `manifest already declares publish unexpectedly: ${manifestPath}`);
  let insertAt = lines.findIndex((line, index) => index > packageStart && index < packageEnd && /^\s*version\s*=/.test(line));
  if (insertAt < 0) insertAt = lines.findIndex((line, index) => index > packageStart && index < packageEnd && /^\s*name\s*=/.test(line));
  invariant(insertAt >= 0, `manifest has no package name/version: ${manifestPath}`);
  lines.splice(insertAt + 1, 0, 'publish = false');
  writeFileSync(manifestPath, lines.join(newline), 'utf8');
}

const write = process.argv.slice(2).includes('--write');
invariant(process.argv.slice(2).every((arg) => arg === '--write' || arg === '--check'), 'usage: node scripts/publish_boundary.mjs [--check|--write]');
const before = metadata();
const violations = boundaryViolations(before);
if (write) {
  for (const pkg of violations) {
    if (!expected.has(pkg.name)) markPrivate(pkg.manifest_path);
  }
}
const after = metadata();
const remaining = boundaryViolations(after);
if (remaining.length > 0) {
  console.error('[publish-boundary] FAILED');
  for (const pkg of remaining) {
    const required = expected.has(pkg.name) ? 'publishable' : 'publish = false';
    console.error(`- ${pkg.name}: expected ${required} (${path.relative(workspaceRoot, pkg.manifest_path)})`);
  }
  process.exitCode = 1;
} else {
  const privateCount = after.packages.filter((pkg) => after.workspace_members.includes(pkg.id) && Array.isArray(pkg.publish) && pkg.publish.length === 0).length;
  console.log(`[publish-boundary] passed: ${expected.size} publishable, ${privateCount} private workspace packages`);
}
