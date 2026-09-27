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

function expectCompileFailure(args, diagnosticMarkers) {
  console.log(`[compile-fail] cargo ${args.join(' ')}`);
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
    throw new Error('expected the facade streaming feature-off consumer to fail compilation');
  }

  const missingMarkers = diagnosticMarkers.filter(marker => !output.includes(marker));
  if (missingMarkers.length > 0) {
    console.error(output);
    throw new Error(`compile failed for an unexpected reason; missing diagnostic markers: ${missingMarkers.join(', ')}`);
  }
  console.log(`[compile-fail] expected facade feature rejection confirmed: ${diagnosticMarkers.join('; ')}`);
}

run(['check', '--manifest-path', 'tests/compile/facade-only/Cargo.toml', '--locked']);
expectCompileFailure([
  'check',
  '--manifest-path',
  'tests/compile/facade-streaming-feature-off/Cargo.toml',
  '--locked'
], ['unresolved imports', 'streaming_transition', 'could not find `streaming` in `core`']);
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
