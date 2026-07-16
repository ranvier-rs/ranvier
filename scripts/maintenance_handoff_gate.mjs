import { existsSync, lstatSync, mkdirSync, readFileSync, realpathSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const workspaceRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const policyPath = path.join(workspaceRoot, '.ranvier-supply-chain-policy.json');
const allowedOutputRoot = path.join(workspaceRoot, 'target', 'supply-chain');
const defaultExercisePath = path.join(
  workspaceRoot,
  'security',
  'handoff-exercises',
  'm419-rq11-20260717.json'
);

function invariant(condition, message) {
  if (!condition) throw new Error(message);
}

function parseInstant(value, field) {
  invariant(/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$/.test(value ?? ''), `${field} must be a canonical UTC instant`);
  const instant = new Date(value);
  invariant(
    Number.isFinite(instant.getTime()) && instant.toISOString() === value.replace('Z', '.000Z'),
    `${field} must be a valid canonical UTC instant`
  );
  return instant;
}

function safeOutputPath(requested) {
  const output = path.resolve(workspaceRoot, requested);
  const relative = path.relative(allowedOutputRoot, output);
  invariant(relative !== '' && !relative.startsWith('..') && !path.isAbsolute(relative), 'output must be a file under target/supply-chain');
  mkdirSync(allowedOutputRoot, { recursive: true });
  invariant(realpathSync(allowedOutputRoot) === allowedOutputRoot, 'supply-chain output root cannot be a symlink');
  let cursor = allowedOutputRoot;
  for (const segment of relative.split(path.sep).slice(0, -1)) {
    cursor = path.join(cursor, segment);
    if (existsSync(cursor)) invariant(!lstatSync(cursor).isSymbolicLink(), `output parent cannot be a symlink: ${cursor}`);
  }
  if (existsSync(output)) invariant(!lstatSync(output).isSymbolicLink(), 'output file cannot be a symlink');
  return output;
}

function eventByType(exercise, type) {
  const matches = exercise.events.filter((event) => event.type === type);
  invariant(matches.length === 1, `exercise must contain exactly one ${type} event`);
  return matches[0];
}

export function evaluateHandoff(policy, exercise, now = new Date()) {
  const errors = [];
  const check = (condition, message) => {
    if (!condition) errors.push(message);
  };

  try {
    invariant(policy.schema_version === '1.0.0', 'unsupported policy schema');
    invariant(exercise.schema_version === '1.0.0', 'unsupported exercise schema');
    invariant(policy.ownership.primary !== policy.ownership.backup, 'primary and backup must be distinct');
    check(exercise.exercise_type === 'tabletop' && exercise.synthetic === true, 'exercise must be an explicit synthetic tabletop');
    check(exercise.scenario?.severity === 'high', 'exercise scenario must use a High advisory');
    check(exercise.primary?.name === policy.ownership.primary, 'exercise primary does not match policy');
    check(exercise.primary?.available === false, 'exercise must model the primary as unavailable');
    check(exercise.backup?.name === policy.ownership.backup, 'exercise backup does not match policy');
    check(exercise.backup?.accepted_role === true, 'backup role acceptance is missing');

    const detected = parseInstant(exercise.scenario?.detected_at, 'scenario.detected_at');
    const started = parseInstant(exercise.started_at, 'started_at');
    const completed = parseInstant(exercise.completed_at, 'completed_at');
    check(started.getTime() >= detected.getTime(), 'exercise starts before detection');
    check(completed.getTime() >= started.getTime(), 'exercise completes before it starts');

    const acknowledged = eventByType(exercise, 'acknowledged');
    const assessed = eventByType(exercise, 'initial_assessment');
    const decision = eventByType(exercise, 'release_decision');
    const closed = eventByType(exercise, 'handoff_closed');
    for (const event of [acknowledged, assessed, decision, closed]) {
      check(event.actor === policy.ownership.backup, `${event.type} must be owned by the backup`);
    }

    const acknowledgedAt = parseInstant(acknowledged.at, 'acknowledged.at');
    const assessedAt = parseInstant(assessed.at, 'initial_assessment.at');
    const decisionAt = parseInstant(decision.at, 'release_decision.at');
    const closedAt = parseInstant(closed.at, 'handoff_closed.at');
    const acknowledgmentHours = (acknowledgedAt - detected) / 3_600_000;
    check(acknowledgmentHours >= 0, 'acknowledgment precedes detection');
    check(
      acknowledgmentHours <= policy.ownership.acknowledgment_hours,
      `acknowledgment exceeds ${policy.ownership.acknowledgment_hours} hours`
    );
    check(assessedAt >= acknowledgedAt, 'assessment precedes acknowledgment');
    check(decisionAt >= assessedAt, 'decision precedes assessment');
    check(closedAt >= decisionAt && closedAt.getTime() === completed.getTime(), 'closure timeline is inconsistent');
    check(assessed.severity === 'high', 'backup assessment must preserve High severity');
    check(decision.decision === 'no-go', 'High advisory exercise must produce release no-go');
    check(typeof decision.reason === 'string' && decision.reason.length >= 20, 'release decision requires a rationale');
    check(typeof closed.next_action === 'string' && closed.next_action.length >= 20, 'handoff closure requires a next action');
    check(Array.isArray(exercise.non_claims) && exercise.non_claims.length >= 3, 'exercise must preserve at least three non-claims');

    const ageDays = (now - completed) / 86_400_000;
    check(ageDays >= 0, 'exercise completion is in the future');
    check(
      ageDays <= policy.ownership.max_handoff_exercise_age_days,
      `exercise is older than ${policy.ownership.max_handoff_exercise_age_days} days`
    );
  } catch (error) {
    errors.push(error.message);
  }

  return { passed: errors.length === 0, errors };
}

function runSelfTest(policy, exercise) {
  const now = new Date('2026-07-17T04:00:00Z');
  invariant(evaluateHandoff(policy, exercise, now).passed, 'valid handoff fixture did not pass');

  const sameOwnerPolicy = structuredClone(policy);
  sameOwnerPolicy.ownership.backup = sameOwnerPolicy.ownership.primary;
  invariant(!evaluateHandoff(sameOwnerPolicy, exercise, now).passed, 'same-owner fixture unexpectedly passed');

  const missingNoGo = structuredClone(exercise);
  eventByType(missingNoGo, 'release_decision').decision = 'go';
  invariant(!evaluateHandoff(policy, missingNoGo, now).passed, 'missing no-go fixture unexpectedly passed');

  const lateAck = structuredClone(exercise);
  eventByType(lateAck, 'acknowledged').at = '2026-07-20T02:00:00Z';
  invariant(!evaluateHandoff(policy, lateAck, new Date('2026-07-20T04:00:00Z')).passed, 'late acknowledgment fixture unexpectedly passed');

  const stale = evaluateHandoff(policy, exercise, new Date('2027-01-17T04:00:00Z'));
  invariant(!stale.passed, 'stale exercise fixture unexpectedly passed');
  const invalidDate = structuredClone(exercise);
  invalidDate.started_at = '2026-02-30T15:30:00Z';
  invariant(!evaluateHandoff(policy, invalidDate, now).passed, 'calendar-invalid timestamp unexpectedly passed');
  console.log('[maintenance-handoff] negative self-test passed');
}

function parseArgs(argv) {
  const args = { selfTest: false, exercise: defaultExercisePath, output: null };
  for (let i = 0; i < argv.length; i += 1) {
    if (argv[i] === '--self-test') args.selfTest = true;
    else if (argv[i] === '--exercise') args.exercise = path.resolve(workspaceRoot, argv[++i]);
    else if (argv[i] === '--output') args.output = path.resolve(workspaceRoot, argv[++i]);
    else throw new Error(`unknown argument: ${argv[i]}`);
  }
  return args;
}

const args = parseArgs(process.argv.slice(2));
const policy = JSON.parse(readFileSync(policyPath, 'utf8'));
const exercise = JSON.parse(readFileSync(args.exercise, 'utf8'));
if (args.selfTest) runSelfTest(policy, exercise);
const result = {
  schema_version: '1.0.0',
  evaluated_at: new Date().toISOString(),
  policy: path.relative(workspaceRoot, policyPath).replaceAll('\\', '/'),
  exercise: path.relative(workspaceRoot, args.exercise).replaceAll('\\', '/'),
  ...evaluateHandoff(policy, exercise)
};
if (args.output) {
  const output = safeOutputPath(args.output);
  mkdirSync(path.dirname(output), { recursive: true });
  writeFileSync(output, `${JSON.stringify(result, null, 2)}\n`, 'utf8');
}
if (!result.passed) {
  console.error(`[maintenance-handoff] FAILED\n${result.errors.map((error) => `- ${error}`).join('\n')}`);
  process.exitCode = 1;
} else {
  console.log(`[maintenance-handoff] passed: ${exercise.exercise_id}`);
}
