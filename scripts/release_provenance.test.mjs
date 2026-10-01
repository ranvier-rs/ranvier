import { packageCohortArgs } from './release_provenance_package_args.mjs';
import { readFileSync } from 'node:fs';
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

test('guard CI uses readonly scope and attestation is excluded from PR events', () => {
  const workflow = readFileSync(new URL('../.github/workflows/release-provenance.yml', import.meta.url), 'utf8');
  const guards = workflow.split('  provenance-guards:')[1].split('  provenance:')[0];
  assert.match(guards, /shell: bash/);
  assert.match(guards, /node --test scripts\/release_provenance.test.mjs/);
  assert.match(guards, /if: always\(\)/);
  assert.match(guards, /persist-credentials: false/);
  assert.match(guards, /provenance-guard-identities.txt/);
  assert.doesNotMatch(guards, /id-token: write|attestations: write/);
  const top = workflow.split('jobs:')[0];
  assert.doesNotMatch(top, /id-token: write|attestations: write/);
  assert.match(top, /pull_request:/);
  assert.match(workflow, /if: github.event_name != 'pull_request'/);
  assert.match(workflow, /needs: provenance-guards/);
});

test('one package command selects exactly the policy cohort', () => {
  const policy = JSON.parse(readFileSync(new URL('../.ranvier-supply-chain-policy.json', import.meta.url), 'utf8'));
  const names = policy.provenance.publishable_crates;
  const args = packageCohortArgs(names);
  assert.equal(names.length, 12);
  assert.deepEqual(args.slice(0, 3), ['package', '--locked', '--no-verify']);
  assert.deepEqual(args.slice(3), names.flatMap(name => ['-p', name]));
  assert.equal(args.filter(value => value === 'package').length, 1);
  assert.deepEqual(packageCohortArgs(['fixture-core', 'fixture-macros']), ['package', '--locked', '--no-verify', '-p', 'fixture-core', '-p', 'fixture-macros']);
});
test('invalid package selections fail before Cargo', () => {
  for (const names of [null, [], ['a', 'a'], ['--workspace'], [''], [undefined]]) {
    assert.throws(() => packageCohortArgs(names), /unique nonempty/);
  }
});
