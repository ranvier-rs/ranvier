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

run(['check', '--manifest-path', 'tests/compile/facade-only/Cargo.toml', '--locked']);
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
