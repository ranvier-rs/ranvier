import assert from 'node:assert/strict';
import { test } from 'node:test';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
const script = fileURLToPath(new URL('./release_provenance.mjs', import.meta.url));
for (const args of [
  ['--verify-crate'], ['--expected-commit'], ['--output'],
  ['--verify-crate', '--expected-commit', 'a'.repeat(40)],
  ['--expected-commit', 'a'.repeat(40)],
  ['--verify-crate', 'absent.crate', '--expected-commit', 'invalid'],
  ['--verify-crate', 'absent.crate', '--expected-commit', 'a'.repeat(40), '--self-test'],
  ['--verify-crate', 'absent.crate', '--expected-commit', 'a'.repeat(40), '--output', 'unused'],
]) {
  test(`invalid crate verification arguments ${JSON.stringify(args)}`, () => {
    const result = spawnSync(process.execPath, [script, ...args], { encoding: 'utf8', timeout: 5000 });
    assert.ifError(result.error);
    assert.equal(result.signal, null);
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /option requires|supplied together|expected-commit must|cannot capture/);
    assert.doesNotMatch(result.stdout, /CRATE_VCS_PASS/);
  });
}
test('tar self-test requires all malformed archive receipts', () => {
  const result = spawnSync(process.execPath, [script, '--self-test'], { encoding: 'utf8', timeout: 5000 });
  assert.ifError(result.error);
  assert.equal(result.status, 0, result.stderr);
  for (const marker of ['truncated', 'checksum', 'invalid-size', 'link']) {
    assert.ok(result.stdout.split(/\r?\n/).includes(`[release-provenance] TAR_GUARD_PASS ${marker}`));
  }
});
