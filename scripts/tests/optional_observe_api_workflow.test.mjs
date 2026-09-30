import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const workflow = readFileSync(new URL('../../.github/workflows/api-compat.yml', import.meta.url), 'utf8').replace(/\r\n?/g, '\n');
const job = workflow.split('  optional-observe-api:\n')[1]?.split('\n  candidate-api:')[0];

test('optional inventory and policy trigger both push and pull request', () => {
  for (const event of ['pull_request', 'push']) {
    const section = workflow.split(`  ${event}:\n`)[1]?.split(/\n  [a-z_]+:/)[0];
    assert.ok(section, event);
    for (const file of ['.ranvier-observe-api-policy.json', 'observe-api-surface-inventory.json']) {
      assert.ok(section.includes(`'${file}'`), `${event}: ${file}`);
    }
  }
});

test('optional API runs independently with exact reviewed compiler and finite job budget', () => {
  assert.ok(job);
  assert.match(job, /timeout-minutes: 30/);
  assert.match(job, /node-version: '24'/);
  assert.match(job, /rust-toolchain@1\.95\.0/);
  assert.match(job, /node scripts\/api_surface_inventory\.mjs --check --optional-observe/);
  assert.doesNotMatch(job, /\bneeds:|continue-on-error:|--write/);
});

test('published baseline lane and read-only permissions are retained', () => {
  assert.match(workflow, /permissions:\n  contents: read/);
  const published = workflow.split('  candidate-api:\n')[1];
  assert.match(published, /node scripts\/api_surface_inventory\.mjs --check\s*\n/);
  assert.match(published, /node scripts\/candidate_api_semver\.mjs/);
});
