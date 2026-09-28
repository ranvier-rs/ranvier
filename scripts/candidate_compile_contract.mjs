#!/usr/bin/env node
import { spawnSync } from 'node:child_process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const workspaceRoot = path.resolve(scriptDir, '..');

function run(args) {
  console.log(`[compile] cargo ${args.join(' ')}`);
  const result = spawnSync('cargo', args, {
    cwd: workspaceRoot,
    env: { ...process.env, CARGO_TARGET_DIR: path.join(workspaceRoot, 'target') },
    stdio: 'inherit'
  });
  if ((result.status ?? 1) !== 0) process.exit(result.status ?? 1);
}

function expectCompileFailure(args, diagnosticMarkers, description) {
  console.log(`[compile-fail] ${description}: cargo ${args.join(' ')}`);
  const result = spawnSync('cargo', args, {
    cwd: workspaceRoot,
    env: { ...process.env, CARGO_TARGET_DIR: path.join(workspaceRoot, 'target') },
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe']
  });
  if (result.error) throw result.error;

  const output = `${result.stdout ?? ''}${result.stderr ?? ''}`;
  if (result.status === 0) {
    console.error(output);
    throw new Error(`expected ${description} to fail compilation`);
  }

  const missingMarkers = diagnosticMarkers.filter(marker => !output.includes(marker));
  if (missingMarkers.length > 0) {
    console.error(output);
    throw new Error(`compile failed for an unexpected reason; missing diagnostic markers: ${missingMarkers.join(', ')}`);
  }
  console.log(`[compile-fail] expected ${description} rejection confirmed: ${diagnosticMarkers.join('; ')}`);
}

run(['check', '--manifest-path', 'tests/compile/facade-only/Cargo.toml', '--locked']);
run([
  'check',
  '--manifest-path',
  'tests/compile/facade-only/Cargo.toml',
  '--features',
  'schema-contract',
  '--locked'
]);
run(['check', '--manifest-path', 'tests/compile/renamed-direct/Cargo.toml', '--locked']);
run(['check', '--manifest-path', 'macros/tests/compile/streaming-accepted/Cargo.toml', '--locked']);
run([
  'check',
  '--package',
  'ranvier-streaming-public-api-consumer',
  '--manifest-path',
  'macros/tests/compile/streaming-public-consumer/Cargo.toml',
  '--locked'
]);
run([
  'check',
  '--package',
  'ranvier-ordinary-public-api-consumer',
  '--manifest-path',
  'macros/tests/compile/streaming-public-consumer/Cargo.toml',
  '--locked'
]);
expectCompileFailure([
  'check',
  '--manifest-path',
  'macros/tests/compile/streaming-public-consumer/restricted-consumer/Cargo.toml',
  '--locked'
], [
  'error[E0603]',
  'constant `crate_scoped` is private'
], 'downstream crate-private streaming transition');
expectCompileFailure([
  'check',
  '--manifest-path',
  'tests/compile/facade-streaming-feature-off/Cargo.toml',
  '--locked'
], [
  'unresolved import',
  'no `streaming_transition` in the root',
  'could not find `streaming` in `core`'
], 'facade streaming feature-off consumer');
expectCompileFailure([
  'check',
  '--manifest-path',
  'tests/compile/direct-macro-streaming-feature-off/Cargo.toml',
  '--locked'
], [
  'unresolved import',
  'no `streaming_transition` in the root',
  'could not find `streaming` in `ranvier_core`'
], 'direct-macro streaming feature-off consumer');
run([
  'check',
  '-p',
  'typed-state-tree',
  '-p',
  'typed-json-api',
  '-p',
  'bus-capability-demo',
  '--locked'
]);
console.log('candidate compile contract: pass');
