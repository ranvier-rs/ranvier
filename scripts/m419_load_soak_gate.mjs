#!/usr/bin/env node

import { spawn, spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { createWriteStream } from 'node:fs';
import {
  access,
  mkdir,
  readFile,
  realpath,
  stat,
  writeFile
} from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import process from 'node:process';
import { fileURLToPath } from 'node:url';
import { performance } from 'node:perf_hooks';
import { promisify } from 'node:util';
import { gzip } from 'node:zlib';

const ENGINE_VERSION = '1.0.0';
const gzipAsync = promisify(gzip);
const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const workspaceRoot = path.resolve(scriptDir, '..');
const defaultPolicy = path.join(workspaceRoot, '.ranvier-load-soak-policy.json');
const defaultOutputRoot = path.join(workspaceRoot, 'target', 'm419-load-soak');

function parseArguments(argv) {
  const options = {
    policy: defaultPolicy,
    output: defaultOutputRoot,
    mode: 'full',
    selfTest: false
  };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    const next = () => {
      index += 1;
      if (index >= argv.length) throw new Error(`missing value for ${argument}`);
      return argv[index];
    };
    switch (argument) {
      case '--policy':
        options.policy = path.resolve(next());
        break;
      case '--output':
        options.output = path.resolve(next());
        break;
      case '--quick':
        options.mode = 'quick';
        break;
      case '--full':
        options.mode = 'full';
        break;
      case '--self-test':
        options.selfTest = true;
        break;
      default:
        throw new Error(`unknown argument: ${argument}`);
    }
  }
  return options;
}

function canonicalText(value) {
  return value.toString('utf8').replace(/^\uFEFF/, '').replace(/\r\n?/g, '\n');
}

async function readJson(file) {
  return JSON.parse(canonicalText(await readFile(file)));
}

function assertFiniteNumber(value, label, minimum = 0) {
  if (!Number.isFinite(value) || value < minimum) {
    throw new Error(`${label} must be a finite number >= ${minimum}`);
  }
}

function assertInteger(value, label, minimum = 0) {
  assertFiniteNumber(value, label, minimum);
  if (!Number.isInteger(value)) throw new Error(`${label} must be an integer`);
}

function validatePolicy(policy) {
  if (policy?.schema_version !== '1.0.0') throw new Error('unsupported policy schema_version');
  if (policy.engine_version !== ENGINE_VERSION) {
    throw new Error(`policy engine_version must be ${ENGINE_VERSION}`);
  }
  if (!policy.fixture || !policy.environment || !policy.memory || !policy.retention || !policy.drain) {
    throw new Error('policy is missing a required section');
  }
  for (const field of ['package', 'binary', 'bind', 'decision_path', 'slow_path', 'stats_path']) {
    if (typeof policy.fixture[field] !== 'string' || policy.fixture[field].length === 0) {
      throw new Error(`fixture.${field} must be a non-empty string`);
    }
  }
  if (!/^127\.0\.0\.1:\d{1,5}$/.test(policy.fixture.bind)) {
    throw new Error('fixture.bind must use an explicit IPv4 loopback port');
  }
  const bindPort = Number(policy.fixture.bind.split(':')[1]);
  if (bindPort < 1 || bindPort > 65_535) throw new Error('fixture.bind port is out of range');
  for (const field of ['decision_path', 'slow_path', 'stats_path']) {
    if (!policy.fixture[field].startsWith('/') || policy.fixture[field].includes('..')) {
      throw new Error(`fixture.${field} must be an absolute, traversal-free URL path`);
    }
  }
  if (!policy.fixture.request_body || typeof policy.fixture.request_body !== 'object') {
    throw new Error('fixture.request_body must be an object');
  }
  for (const field of ['os', 'architecture', 'rustc']) {
    if (typeof policy.environment[field] !== 'string' || policy.environment[field].length === 0) {
      throw new Error(`environment.${field} must be a non-empty string`);
    }
  }
  assertFiniteNumber(policy.environment.container_cpus, 'environment.container_cpus', 0.1);
  assertInteger(policy.environment.container_memory_mib, 'environment.container_memory_mib', 1);
  assertInteger(policy.environment.container_pids, 'environment.container_pids', 1);
  assertInteger(policy.environment.node_major, 'environment.node_major', 1);
  if (!Array.isArray(policy.phases) || policy.phases.length !== 4) {
    throw new Error('policy must define exactly four phases');
  }
  const expectedNames = ['warmup', 'steady', 'burst', 'soak'];
  policy.phases.forEach((phase, index) => {
    if (phase.name !== expectedNames[index]) throw new Error(`phase ${index} must be ${expectedNames[index]}`);
    if (phase.measured !== (index > 0)) {
      throw new Error(`${phase.name}.measured must be ${index > 0}`);
    }
    assertInteger(phase.duration_seconds, `${phase.name}.duration_seconds`, 1);
    assertInteger(phase.concurrency, `${phase.name}.concurrency`, 1);
    if (phase.measured) {
      assertInteger(phase.minimum_successes, `${phase.name}.minimum_successes`, 1);
      assertFiniteNumber(phase.maximum_error_percent, `${phase.name}.maximum_error_percent`);
      if (phase.maximum_error_percent > 100) {
        throw new Error(`${phase.name}.maximum_error_percent must be <= 100`);
      }
      assertFiniteNumber(phase.maximum_p95_ms, `${phase.name}.maximum_p95_ms`, 1);
      assertFiniteNumber(phase.maximum_p99_ms, `${phase.name}.maximum_p99_ms`, 1);
      if (phase.maximum_p95_ms > phase.maximum_p99_ms) {
        throw new Error(`${phase.name}.maximum_p95_ms must be <= maximum_p99_ms`);
      }
    }
  });
  assertInteger(policy.request_timeout_ms, 'request_timeout_ms', 1);
  for (const [field, value] of Object.entries(policy.memory)) {
    assertFiniteNumber(value, `memory.${field}`, 1);
  }
  assertInteger(policy.memory.sample_interval_ms, 'memory.sample_interval_ms', 1);
  assertInteger(policy.memory.slope_window_seconds, 'memory.slope_window_seconds', 1);
  const soak = policy.phases.find((phase) => phase.name === 'soak');
  if (policy.memory.slope_window_seconds > soak.duration_seconds) {
    throw new Error('memory.slope_window_seconds must fit inside the soak phase');
  }
  for (const [field, value] of Object.entries(policy.retention)) {
    if (field === 'require_eviction') {
      if (value !== true) throw new Error('retention.require_eviction must remain true');
    } else {
      assertInteger(value, `retention.${field}`, 1);
    }
  }
  assertInteger(policy.drain.signal_delay_ms, 'drain.signal_delay_ms', 1);
  assertInteger(policy.drain.maximum_elapsed_ms, 'drain.maximum_elapsed_ms', 1);
  assertInteger(policy.drain.expected_status, 'drain.expected_status', 100);
  if (policy.drain.expected_status > 599) throw new Error('drain.expected_status must be <= 599');
  if (policy.drain.signal_delay_ms >= policy.drain.maximum_elapsed_ms) {
    throw new Error('drain.signal_delay_ms must be below drain.maximum_elapsed_ms');
  }
  return policy;
}

function isInside(parent, candidate) {
  const relative = path.relative(parent, candidate);
  return relative === '' || (!relative.startsWith('..') && !path.isAbsolute(relative));
}

function validateOutputPath(output) {
  const explicitRoot = process.env.RANVIER_RQ10_OUTPUT_ROOT
    ? path.resolve(process.env.RANVIER_RQ10_OUTPUT_ROOT)
    : defaultOutputRoot;
  if (!isInside(explicitRoot, output)) {
    throw new Error(`output must remain inside ${explicitRoot}`);
  }
  return explicitRoot;
}

async function validateResolvedOutputPath(root, output) {
  const [resolvedRoot, resolvedOutput] = await Promise.all([realpath(root), realpath(output)]);
  if (!isInside(resolvedRoot, resolvedOutput)) {
    throw new Error(`resolved output must remain inside ${resolvedRoot}`);
  }
}

function commandVersion(command, args = ['--version']) {
  const result = spawnSync(command, args, { encoding: 'utf8' });
  if ((result.status ?? 1) !== 0) return null;
  return result.stdout.trim();
}

async function readOptional(file) {
  try {
    return canonicalText(await readFile(file)).trim();
  } catch {
    return null;
  }
}

async function collectEnvironment() {
  const cpuInfo = await readOptional('/proc/cpuinfo');
  const cpuModel = cpuInfo
    ?.split('\n')
    .find((line) => line.startsWith('model name'))
    ?.split(':')
    .slice(1)
    .join(':')
    .trim() ?? os.cpus()[0]?.model ?? null;
  return {
    platform: process.platform,
    architecture: process.arch,
    kernel: os.release(),
    cpu_model: cpuModel,
    logical_cpus_visible: os.cpus().length,
    cgroup_cpu_max: await readOptional('/sys/fs/cgroup/cpu.max'),
    cgroup_memory_max: await readOptional('/sys/fs/cgroup/memory.max'),
    cgroup_pids_max: await readOptional('/sys/fs/cgroup/pids.max'),
    container_image_id: process.env.RANVIER_RQ10_IMAGE_ID ?? null,
    container_image_digest: process.env.RANVIER_RQ10_IMAGE_DIGEST ?? null,
    source_state: process.env.RANVIER_RQ10_SOURCE_STATE ?? null,
    rustc: commandVersion('rustc'),
    cargo: commandVersion('cargo'),
    node: process.version
  };
}

function validateEnvironment(policy, environment, mode) {
  const failures = [];
  if (environment.platform !== policy.environment.os) failures.push(`platform=${environment.platform}`);
  if (environment.architecture !== policy.environment.architecture) {
    failures.push(`architecture=${environment.architecture}`);
  }
  if (!environment.rustc?.startsWith(`rustc ${policy.environment.rustc} `)) {
    failures.push(`rustc=${environment.rustc ?? 'missing'}`);
  }
  if (Number(process.versions.node.split('.')[0]) !== policy.environment.node_major) {
    failures.push(`node=${process.version}`);
  }
  const [quotaText, periodText] = (environment.cgroup_cpu_max ?? '').split(/\s+/);
  const quota = Number(quotaText);
  const period = Number(periodText);
  if (!Number.isFinite(quota) || !Number.isFinite(period) || Math.abs(quota / period - policy.environment.container_cpus) > 0.05) {
    failures.push(`cgroup_cpu_max=${environment.cgroup_cpu_max ?? 'missing'}`);
  }
  const memoryBytes = Number(environment.cgroup_memory_max);
  if (memoryBytes !== policy.environment.container_memory_mib * 1024 * 1024) {
    failures.push(`cgroup_memory_max=${environment.cgroup_memory_max ?? 'missing'}`);
  }
  if (Number(environment.cgroup_pids_max) !== policy.environment.container_pids) {
    failures.push(`cgroup_pids_max=${environment.cgroup_pids_max ?? 'missing'}`);
  }
  if (!/^[0-9a-f]{64}$/.test(environment.container_image_id ?? '')) {
    failures.push(`container_image_id=${environment.container_image_id ?? 'missing'}`);
  }
  if (!/^sha256:[0-9a-f]{64}$/.test(environment.container_image_digest ?? '')) {
    failures.push(`container_image_digest=${environment.container_image_digest ?? 'missing'}`);
  }
  if (environment.source_state !== 'clean') {
    failures.push(`source_state=${environment.source_state ?? 'missing'}`);
  }
  if (mode === 'full' && failures.length > 0) {
    throw new Error(`canonical environment mismatch: ${failures.join(', ')}`);
  }
  return failures;
}

async function runLogged(command, args, { cwd, env, output }) {
  return await new Promise((resolve, reject) => {
    const stream = createWriteStream(output, { flags: 'w' });
    const child = spawn(command, args, { cwd, env, stdio: ['ignore', 'pipe', 'pipe'] });
    child.stdout.pipe(stream, { end: false });
    child.stderr.pipe(stream, { end: false });
    child.on('error', reject);
    child.on('exit', (code, signal) => {
      stream.end(() => {
        if (code === 0) resolve();
        else reject(new Error(`${command} exited code=${code} signal=${signal ?? 'none'}; see ${output}`));
      });
    });
  });
}

function delay(milliseconds) {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

function waitForStreamFinish(stream) {
  if (stream.closed) return Promise.resolve();
  return new Promise((resolve, reject) => {
    stream.once('close', resolve);
    stream.once('error', reject);
  });
}

async function waitForReady(baseUrl, child, timeoutMs = 30_000) {
  const deadline = performance.now() + timeoutMs;
  while (performance.now() < deadline) {
    if (child.exitCode !== null) throw new Error(`server exited before readiness with ${child.exitCode}`);
    try {
      const response = await fetch(`${baseUrl}/ready`, { signal: AbortSignal.timeout(1000) });
      if (response.status === 200) return;
    } catch {
      // Bounded readiness polling owns this transient failure.
    }
    await delay(100);
  }
  throw new Error('server readiness timed out');
}

function percentile(sortedValues, fraction) {
  if (sortedValues.length === 0) return null;
  const rank = Math.max(0, Math.ceil(fraction * sortedValues.length) - 1);
  return sortedValues[rank];
}

async function issueDecision(baseUrl, policy) {
  const started = performance.now();
  try {
    const response = await fetch(`${baseUrl}${policy.fixture.decision_path}`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(policy.fixture.request_body),
      signal: AbortSignal.timeout(policy.request_timeout_ms)
    });
    const body = await response.text();
    const elapsed = performance.now() - started;
    let jsonError = null;
    try {
      JSON.parse(body);
    } catch (error) {
      jsonError = `malformed JSON: ${error.message}`;
    }
    const success = response.status >= 200 && response.status < 300 && jsonError == null;
    return {
      elapsed,
      success,
      status: response.status,
      error: success ? null : (jsonError ?? `status ${response.status}`)
    };
  } catch (error) {
    return { elapsed: performance.now() - started, success: false, status: null, error: error.message };
  }
}

async function runPhase(baseUrl, phase, policy, setPhase) {
  setPhase(phase.name);
  const started = performance.now();
  const deadline = started + phase.duration_seconds * 1000;
  const latencies = [];
  const statuses = {};
  const errors = {};
  let successes = 0;
  let failures = 0;

  async function worker() {
    while (performance.now() < deadline) {
      const result = await issueDecision(baseUrl, policy);
      latencies.push(result.elapsed);
      if (result.success) successes += 1;
      else failures += 1;
      const statusKey = result.status == null ? 'network' : String(result.status);
      statuses[statusKey] = (statuses[statusKey] ?? 0) + 1;
      if (result.error) errors[result.error] = (errors[result.error] ?? 0) + 1;
    }
  }

  await Promise.all(Array.from({ length: phase.concurrency }, () => worker()));
  const elapsedSeconds = (performance.now() - started) / 1000;
  const total = successes + failures;
  setPhase(`${phase.name}-post`);
  const sortedLatencies = latencies.sort((left, right) => left - right);
  return {
    name: phase.name,
    configured_duration_seconds: phase.duration_seconds,
    elapsed_seconds: elapsedSeconds,
    concurrency: phase.concurrency,
    successes,
    failures,
    total,
    error_percent: total === 0 ? 100 : (failures / total) * 100,
    requests_per_second: total / elapsedSeconds,
    latency_ms: {
      p50: percentile(sortedLatencies, 0.50),
      p95: percentile(sortedLatencies, 0.95),
      p99: percentile(sortedLatencies, 0.99),
      maximum: sortedLatencies.length === 0
        ? null
        : sortedLatencies.at(-1)
    },
    latency_samples_ms: sortedLatencies,
    statuses,
    errors
  };
}

async function readRssMib(pid) {
  const status = await readFile(`/proc/${pid}/status`, 'utf8');
  const match = status.match(/^VmRSS:\s+(\d+)\s+kB$/m);
  if (!match) throw new Error(`VmRSS missing for pid ${pid}`);
  return Number(match[1]) / 1024;
}

function startMemorySampler(pid, intervalMs, getPhase) {
  const samples = [];
  const errors = [];
  const started = performance.now();
  let stopped = false;
  let timer;
  let inFlight = Promise.resolve();
  let nextDue = performance.now() + intervalMs;
  const schedule = () => {
    const waitMs = Math.max(0, nextDue - performance.now());
    timer = setTimeout(() => {
      inFlight = sample();
    }, waitMs);
  };
  const sample = async () => {
    if (stopped) return;
    try {
      samples.push({
        elapsed_ms: performance.now() - started,
        phase: getPhase(),
        rss_mib: await readRssMib(pid)
      });
    } catch (error) {
      errors.push(error.message);
    }
    if (!stopped) {
      nextDue += intervalMs;
      if (nextDue <= performance.now()) nextDue = performance.now() + intervalMs;
      schedule();
    }
  };
  schedule();
  return {
    async stop() {
      stopped = true;
      if (timer) clearTimeout(timer);
      await inFlight;
    },
    samples,
    errors
  };
}

async function fetchJson(url, timeoutMs = 3000) {
  const response = await fetch(url, { signal: AbortSignal.timeout(timeoutMs) });
  if (!response.ok) throw new Error(`${url} returned ${response.status}`);
  return await response.json();
}

function waitForExit(child, timeoutMs) {
  if (child.exitCode !== null) return Promise.resolve({ code: child.exitCode, signal: child.signalCode });
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('server exit timed out')), timeoutMs);
    child.once('exit', (code, signal) => {
      clearTimeout(timer);
      resolve({ code, signal });
    });
  });
}

async function runDrain(baseUrl, child, policy) {
  const exit = waitForExit(child, policy.drain.maximum_elapsed_ms + 2000);
  const slowRequest = fetch(`${baseUrl}${policy.fixture.slow_path}`, {
    signal: AbortSignal.timeout(policy.drain.maximum_elapsed_ms + 2000)
  }).then(async (response) => {
    await response.arrayBuffer();
    return { status: response.status, error: null };
  }).catch((error) => ({ status: null, error: error.message }));
  const inFlightDeadline = performance.now() + 1000;
  let slowStarted = false;
  while (performance.now() < inFlightDeadline) {
    try {
      const stats = await fetchJson(`${baseUrl}${policy.fixture.stats_path}`, 500);
      if ((stats.slow_started ?? 0) > 0) {
        slowStarted = true;
        break;
      }
    } catch {
      // The bounded poll below reports a deterministic failure if the request
      // never enters the slow transition.
    }
    await delay(20);
  }
  if (!slowStarted) throw new Error('slow request did not enter the transition before shutdown');
  await delay(policy.drain.signal_delay_ms);
  const started = performance.now();
  process.kill(child.pid, 'SIGTERM');
  const [request, processExit] = await Promise.all([slowRequest, exit]);
  return {
    elapsed_ms: performance.now() - started,
    status: request.status,
    request_error: request.error,
    slow_started_before_signal: slowStarted,
    exit_code: processExit.code,
    exit_signal: processExit.signal
  };
}

function linearSlopeMibPerMinute(samples) {
  if (samples.length < 2) return null;
  const origin = samples[0].elapsed_ms;
  const points = samples.map((sample) => ({
    x: (sample.elapsed_ms - origin) / 60_000,
    y: sample.rss_mib
  }));
  const meanX = points.reduce((sum, point) => sum + point.x, 0) / points.length;
  const meanY = points.reduce((sum, point) => sum + point.y, 0) / points.length;
  const numerator = points.reduce((sum, point) => sum + (point.x - meanX) * (point.y - meanY), 0);
  const denominator = points.reduce((sum, point) => sum + (point.x - meanX) ** 2, 0);
  return denominator === 0 ? 0 : numerator / denominator;
}

function summarizeMemory(samples, policy) {
  const steadyAndLater = samples.filter((sample) => ['steady', 'burst', 'soak'].includes(sample.phase));
  const soak = samples.filter((sample) => sample.phase === 'soak');
  const baseline = steadyAndLater[0]?.rss_mib ?? null;
  const final = steadyAndLater.at(-1)?.rss_mib ?? null;
  const peak = steadyAndLater.length > 0 ? Math.max(...steadyAndLater.map((sample) => sample.rss_mib)) : null;
  const quarterCount = Math.max(1, Math.floor(soak.length / 4));
  const lastQuarter = soak.slice(-quarterCount);
  const lastQuarterRange = lastQuarter.length > 0
    ? Math.max(...lastQuarter.map((sample) => sample.rss_mib)) - Math.min(...lastQuarter.map((sample) => sample.rss_mib))
    : null;
  const slopeCutoff = (soak.at(-1)?.elapsed_ms ?? 0) - policy.memory.slope_window_seconds * 1000;
  const slopeSamples = soak.filter((sample) => sample.elapsed_ms >= slopeCutoff);
  const gaps = soak.slice(1).map((sample, index) => sample.elapsed_ms - soak[index].elapsed_ms);
  return {
    sample_count: samples.length,
    soak_sample_count: soak.length,
    baseline_mib: baseline,
    final_mib: final,
    peak_mib: peak,
    final_growth_mib: baseline == null || final == null ? null : final - baseline,
    last_quarter_range_mib: lastQuarterRange,
    final_slope_mib_per_minute: linearSlopeMibPerMinute(slopeSamples),
    maximum_sample_gap_ms: gaps.length === 0 ? null : Math.max(...gaps)
  };
}

function addCheck(checks, name, passed, observed, expected) {
  checks.push({ name, passed: Boolean(passed), observed, expected });
}

function evaluate(policy, result, { requireFull = true } = {}) {
  const checks = [];
  for (const phasePolicy of policy.phases.filter((phase) => phase.measured)) {
    const phase = result.phases.find((candidate) => candidate.name === phasePolicy.name);
    addCheck(checks, `${phasePolicy.name}.minimum_successes`, phase?.successes >= phasePolicy.minimum_successes, phase?.successes, `>= ${phasePolicy.minimum_successes}`);
    addCheck(checks, `${phasePolicy.name}.error_percent`, phase?.error_percent <= phasePolicy.maximum_error_percent, phase?.error_percent, `<= ${phasePolicy.maximum_error_percent}`);
    addCheck(checks, `${phasePolicy.name}.p95_ms`, phase?.latency_ms?.p95 <= phasePolicy.maximum_p95_ms, phase?.latency_ms?.p95, `<= ${phasePolicy.maximum_p95_ms}`);
    addCheck(checks, `${phasePolicy.name}.p99_ms`, phase?.latency_ms?.p99 <= phasePolicy.maximum_p99_ms, phase?.latency_ms?.p99, `<= ${phasePolicy.maximum_p99_ms}`);
  }
  const memory = result.memory.summary;
  const soakPolicy = policy.phases.find((phase) => phase.name === 'soak');
  const minimumSoakSamples = Math.max(
    2,
    Math.floor(soakPolicy.duration_seconds * 1000 / policy.memory.sample_interval_ms) - 1
  );
  addCheck(checks, 'memory.sampler_errors', result.memory.errors.length === 0, result.memory.errors, 'none');
  addCheck(checks, 'memory.soak_sample_count', memory.soak_sample_count >= minimumSoakSamples, memory.soak_sample_count, `>= ${minimumSoakSamples}`);
  addCheck(checks, 'memory.sample_gap_ms', memory.maximum_sample_gap_ms != null && memory.maximum_sample_gap_ms <= policy.memory.sample_interval_ms * 2.5, memory.maximum_sample_gap_ms, `<= ${policy.memory.sample_interval_ms * 2.5}`);
  addCheck(checks, 'memory.peak_mib', memory.peak_mib != null && memory.peak_mib <= policy.memory.maximum_peak_mib, memory.peak_mib, `<= ${policy.memory.maximum_peak_mib}`);
  addCheck(checks, 'memory.final_growth_mib', memory.final_growth_mib != null && memory.final_growth_mib <= policy.memory.maximum_final_growth_mib, memory.final_growth_mib, `<= ${policy.memory.maximum_final_growth_mib}`);
  addCheck(checks, 'memory.last_quarter_range_mib', memory.last_quarter_range_mib != null && memory.last_quarter_range_mib <= policy.memory.maximum_last_quarter_range_mib, memory.last_quarter_range_mib, `<= ${policy.memory.maximum_last_quarter_range_mib}`);
  addCheck(checks, 'memory.final_slope_mib_per_minute', memory.final_slope_mib_per_minute != null && memory.final_slope_mib_per_minute <= policy.memory.maximum_slope_mib_per_minute, memory.final_slope_mib_per_minute, `<= ${policy.memory.maximum_slope_mib_per_minute}`);

  const final = result.server_final;
  addCheck(checks, 'trace.active_count', final?.trace?.active_count === 0, final?.trace?.active_count, '0');
  addCheck(checks, 'trace.recent_count', final?.trace?.recent_count <= policy.retention.trace_max_count, final?.trace?.recent_count, `<= ${policy.retention.trace_max_count}`);
  addCheck(checks, 'trace.capacity_evicted', !requireFull || final?.trace?.capacity_evicted > 0, final?.trace?.capacity_evicted, requireFull ? '> 0' : 'diagnostic');
  addCheck(checks, 'events.current_len', final?.events?.current_len <= policy.retention.event_max_count, final?.events?.current_len, `<= ${policy.retention.event_max_count}`);
  addCheck(checks, 'events.dropped_oldest', !requireFull || final?.events?.dropped_oldest > 0, final?.events?.dropped_oldest, requireFull ? '> 0' : 'diagnostic');
  const metricNodes = final?.metrics?.nodes ? Object.values(final.metrics.nodes) : [];
  addCheck(checks, 'metrics.node_count', metricNodes.length >= 3, metricNodes.length, '>= 3');
  addCheck(checks, 'metrics.current_samples', metricNodes.length > 0 && metricNodes.every((node) => node.current_samples <= policy.retention.metric_max_samples_per_node), metricNodes.map((node) => node.current_samples), `each <= ${policy.retention.metric_max_samples_per_node}`);
  addCheck(checks, 'metrics.capacity_evicted', !requireFull || metricNodes.some((node) => node.capacity_evicted > 0), metricNodes.map((node) => node.capacity_evicted), requireFull ? 'some > 0' : 'diagnostic');
  addCheck(checks, 'audit.current_len', final?.audit?.current_len <= policy.retention.audit_max_count, final?.audit?.current_len, `<= ${policy.retention.audit_max_count}`);
  addCheck(checks, 'audit.expired', !requireFull || final?.audit?.expired > 0, final?.audit?.expired, requireFull ? '> 0' : 'diagnostic');
  addCheck(checks, 'drain.status', result.drain.status === policy.drain.expected_status, result.drain.status, String(policy.drain.expected_status));
  addCheck(checks, 'drain.slow_started_before_signal', result.drain.slow_started_before_signal === true, result.drain.slow_started_before_signal, 'true');
  addCheck(checks, 'drain.elapsed_ms', result.drain.elapsed_ms <= policy.drain.maximum_elapsed_ms, result.drain.elapsed_ms, `<= ${policy.drain.maximum_elapsed_ms}`);
  addCheck(checks, 'drain.exit_code', result.drain.exit_code === 0 && result.drain.exit_signal == null, result.drain, 'code 0, no signal');
  const latencyArtifact = result.artifacts?.latency_samples;
  const phaseRequestCount = result.phases.reduce((total, phase) => total + phase.total, 0);
  addCheck(checks, 'artifact.latency_encoding', latencyArtifact?.encoding === 'gzip-json', latencyArtifact?.encoding, 'gzip-json');
  addCheck(checks, 'artifact.latency_sha256', /^[0-9a-f]{64}$/.test(latencyArtifact?.sha256 ?? ''), latencyArtifact?.sha256, '64 lowercase hex characters');
  addCheck(checks, 'artifact.latency_sample_count', latencyArtifact?.sample_count === phaseRequestCount, latencyArtifact?.sample_count, String(phaseRequestCount));
  addCheck(checks, 'artifact.latency_bytes', latencyArtifact?.compressed_bytes > 0 && latencyArtifact?.uncompressed_bytes >= latencyArtifact?.compressed_bytes, latencyArtifact, 'nonempty compressed artifact');
  return { passed: checks.every((check) => check.passed), checks };
}

function quickPolicy(policy) {
  return {
    ...policy,
    phases: policy.phases.map((phase) => ({
      ...phase,
      duration_seconds: phase.name === 'soak' ? 3 : 2,
      concurrency: Math.min(phase.concurrency, 8),
      ...(phase.measured ? { minimum_successes: 1 } : {})
    })),
    memory: {
      ...policy.memory,
      slope_window_seconds: 2
    }
  };
}

function markdownSummary(result) {
  const phaseRows = result.phases
    .map((phase) => `| ${phase.name} | ${phase.successes} | ${phase.failures} | ${phase.error_percent.toFixed(4)}% | ${phase.latency_ms.p95?.toFixed(3) ?? '-'} | ${phase.latency_ms.p99?.toFixed(3) ?? '-'} |`)
    .join('\n');
  const failed = result.evaluation.checks.filter((check) => !check.passed);
  return `# M419-RQ10 Load/Soak Result\n\n` +
    `- Status: **${result.evaluation.passed ? 'PASS' : 'FAIL'}**\n` +
    `- Mode: \`${result.mode}\`\n` +
    `- Evidence eligible: \`${result.evidence_eligible}\`\n` +
    `- Ranvier commit: \`${result.source_commit}\`\n` +
    `- Started: ${result.started_at}\n` +
    `- Completed: ${result.completed_at}\n\n` +
    `| Phase | Success | Failure | Error | p95 ms | p99 ms |\n|---|---:|---:|---:|---:|---:|\n${phaseRows}\n\n` +
    `## Memory\n\n\`\`\`json\n${JSON.stringify(result.memory.summary, null, 2)}\n\`\`\`\n\n` +
    `## Retention and Drain\n\n\`\`\`json\n${JSON.stringify({ final: result.server_final, drain: result.drain }, null, 2)}\n\`\`\`\n\n` +
    `## Raw Artifacts\n\n\`\`\`json\n${JSON.stringify(result.artifacts, null, 2)}\n\`\`\`\n\n` +
    `## Failed Checks\n\n${failed.length === 0 ? 'None.\n' : failed.map((check) => `- ${check.name}: observed ${JSON.stringify(check.observed)}, expected ${check.expected}`).join('\n') + '\n'}`;
}

async function selfTest() {
  const policy = validatePolicy(await readJson(defaultPolicy));
  const malformedPolicy = structuredClone(policy);
  malformedPolicy.phases[1].measured = false;
  let malformedRejected = false;
  try {
    validatePolicy(malformedPolicy);
  } catch {
    malformedRejected = true;
  }
  if (!malformedRejected) throw new Error('malformed measured-phase policy passed');
  const synthetic = {
    phases: policy.phases.filter((phase) => phase.measured).map((phase) => ({
      name: phase.name,
      successes: phase.minimum_successes,
      total: phase.minimum_successes,
      error_percent: 0,
      latency_ms: { p95: 1, p99: 2 }
    })),
    memory: {
      errors: [],
      summary: {
        soak_sample_count: 300,
        maximum_sample_gap_ms: 1000,
        peak_mib: 100,
        final_growth_mib: 1,
        last_quarter_range_mib: 1,
        final_slope_mib_per_minute: 0
      }
    },
    server_final: {
      trace: { active_count: 0, recent_count: 10, capacity_evicted: 1 },
      events: { current_len: 10, dropped_oldest: 1 },
      metrics: { nodes: {
        validate: { current_samples: 10, capacity_evicted: 1 },
        evaluate: { current_samples: 10, capacity_evicted: 1 },
        record: { current_samples: 10, capacity_evicted: 1 }
      } },
      audit: { current_len: 10, expired: 1 }
    },
    drain: {
      status: 503,
      elapsed_ms: 100,
      slow_started_before_signal: true,
      exit_code: 0,
      exit_signal: null
    },
    artifacts: {
      latency_samples: {
        encoding: 'gzip-json',
        sha256: 'a'.repeat(64),
        sample_count: policy.phases
          .filter((phase) => phase.measured)
          .reduce((total, phase) => total + phase.minimum_successes, 0),
        uncompressed_bytes: 200,
        compressed_bytes: 100
      }
    }
  };
  if (!evaluate(policy, synthetic).passed) throw new Error('passing synthetic result was rejected');
  synthetic.phases[0].latency_ms.p99 = policy.phases[1].maximum_p99_ms + 1;
  if (evaluate(policy, synthetic).passed) throw new Error('high latency synthetic result passed');
  let containmentFailed = false;
  try {
    validateOutputPath(path.resolve(defaultOutputRoot, '..', 'unsafe-output'));
  } catch {
    containmentFailed = true;
  }
  if (!containmentFailed) throw new Error('unsafe output containment self-test passed');
  console.log('M419 load/soak gate self-test: pass');
}

async function main() {
  const options = parseArguments(process.argv.slice(2));
  if (options.selfTest) {
    await selfTest();
    return;
  }
  const outputRoot = validateOutputPath(options.output);
  await mkdir(options.output, { recursive: true });
  await validateResolvedOutputPath(outputRoot, options.output);
  const resultPath = path.join(options.output, 'result.json');
  try {
    await access(resultPath);
    throw new Error(`refusing to overwrite existing result: ${resultPath}`);
  } catch (error) {
    if (error.code !== 'ENOENT') throw error;
  }

  const canonicalPolicy = validatePolicy(await readJson(options.policy));
  const policy = options.mode === 'quick' ? quickPolicy(canonicalPolicy) : canonicalPolicy;
  const environment = await collectEnvironment();
  const environmentMismatches = validateEnvironment(canonicalPolicy, environment, options.mode);
  const sourceCommit = process.env.RANVIER_RQ10_SOURCE_COMMIT
    ?? commandVersion('git', ['rev-parse', 'HEAD']);
  if (!/^[0-9a-f]{40}$/.test(sourceCommit ?? '')) {
    throw new Error('RANVIER source commit must be a full 40-character lowercase SHA-1');
  }
  const targetDir = path.resolve(process.env.CARGO_TARGET_DIR ?? path.join(workspaceRoot, 'target'));
  const binaryName = process.platform === 'win32'
    ? `${policy.fixture.binary}.exe`
    : policy.fixture.binary;
  const binary = path.join(targetDir, 'release', binaryName);
  const buildLog = path.join(options.output, 'build.log');
  await runLogged('cargo', [
    'build', '--release', '--locked', '-p', policy.fixture.package, '--bin', policy.fixture.binary
  ], {
    cwd: workspaceRoot,
    env: process.env,
    output: buildLog
  });
  await stat(binary);

  const stdoutPath = path.join(options.output, 'server.stdout.log');
  const stderrPath = path.join(options.output, 'server.stderr.log');
  const serverSummaryPath = path.join(options.output, 'server-summary.json');
  const stdout = createWriteStream(stdoutPath, { flags: 'w' });
  const stderr = createWriteStream(stderrPath, { flags: 'w' });
  const server = spawn(binary, [], {
    cwd: workspaceRoot,
    env: {
      ...process.env,
      RANVIER_RQ10_BIND: policy.fixture.bind,
      RANVIER_RQ10_SERVER_SUMMARY: serverSummaryPath
    },
    stdio: ['ignore', 'pipe', 'pipe']
  });
  const spawnFailure = new Promise((_, reject) => server.once('error', reject));
  server.stdout.pipe(stdout);
  server.stderr.pipe(stderr);
  const baseUrl = `http://${policy.fixture.bind}`;
  let activePhase = 'startup';
  const sampler = startMemorySampler(server.pid, policy.memory.sample_interval_ms, () => activePhase);
  const startedAt = new Date().toISOString();

  let completed = false;
  try {
    await Promise.race([waitForReady(baseUrl, server), spawnFailure]);
    const phaseRuns = [];
    for (const phase of policy.phases) {
      phaseRuns.push(await runPhase(baseUrl, phase, policy, (name) => { activePhase = name; }));
    }
    const latencySamples = Object.fromEntries(
      phaseRuns.map((phase) => [phase.name, phase.latency_samples_ms])
    );
    const phases = phaseRuns.map(({ latency_samples_ms: _samples, ...summary }) => summary);
    activePhase = 'pre-drain';
    const serverBeforeDrain = await fetchJson(`${baseUrl}${policy.fixture.stats_path}`);
    await sampler.stop();
    const drain = await runDrain(baseUrl, server, policy);
    await Promise.all([waitForStreamFinish(stdout), waitForStreamFinish(stderr)]);
    const serverFinal = await readJson(serverSummaryPath);
    const memorySummary = summarizeMemory(sampler.samples, policy);
    const latencyRaw = Buffer.from(`${JSON.stringify({
      schema_version: '1.0.0',
      unit: 'milliseconds',
      ordering: 'ascending',
      phases: latencySamples
    })}\n`, 'utf8');
    const latencyCompressed = await gzipAsync(latencyRaw, { level: 9 });
    const latencyArtifactName = 'latency-samples.json.gz';
    await writeFile(path.join(options.output, latencyArtifactName), latencyCompressed);
    const artifacts = {
      latency_samples: {
        path: latencyArtifactName,
        encoding: 'gzip-json',
        sha256: createHash('sha256').update(latencyCompressed).digest('hex'),
        sample_count: Object.values(latencySamples)
          .reduce((total, samples) => total + samples.length, 0),
        uncompressed_bytes: latencyRaw.byteLength,
        compressed_bytes: latencyCompressed.byteLength
      }
    };
    const result = {
      schema_version: '1.0.0',
      engine_version: ENGINE_VERSION,
      mode: options.mode,
      evidence_eligible: options.mode === 'full' && environmentMismatches.length === 0,
      source_commit: sourceCommit,
      policy: canonicalPolicy,
      execution_policy: policy,
      environment,
      environment_mismatches: environmentMismatches,
      started_at: startedAt,
      completed_at: new Date().toISOString(),
      phases,
      memory: { summary: memorySummary, samples: sampler.samples, errors: sampler.errors },
      server_before_drain: serverBeforeDrain,
      server_final: serverFinal,
      drain,
      artifacts
    };
    result.evaluation = evaluate(policy, result, { requireFull: options.mode === 'full' });
    if (!result.evidence_eligible && options.mode === 'full') {
      result.evaluation.passed = false;
      result.evaluation.checks.push({
        name: 'environment.evidence_eligible',
        passed: false,
        observed: environmentMismatches,
        expected: 'canonical environment'
      });
    }
    await writeFile(resultPath, `${JSON.stringify(result, null, 2)}\n`, 'utf8');
    await writeFile(path.join(options.output, 'summary.md'), markdownSummary(result), 'utf8');
    completed = true;
    if (!result.evaluation.passed) {
      throw new Error(`load/soak acceptance failed; see ${resultPath}`);
    }
    console.log(`M419 load/soak gate: pass (${options.mode})`);
    console.log(`result: ${resultPath}`);
  } finally {
    await sampler.stop();
    if (!completed && server.exitCode === null) {
      try {
        process.kill(server.pid, 'SIGKILL');
      } catch {
        // The owned process may have exited between the check and signal.
      }
    }
  }
}

main().catch((error) => {
  console.error(`M419 load/soak gate: FAILED\n${error.stack ?? error.message}`);
  process.exitCode = 1;
});
