// Maintenance continuity gate for the single-maintainer model.
//
// Ranvier has one human maintainer and no backup. Instead of a backup handoff,
// the maintainer runs a synthetic High-advisory tabletop drill at least every
// `ownership.continuity_drill_max_age_days` days, and a separate-session AI
// review (no access to the drill conversation) checks the record before the
// maintainer approves it. This gate verifies the policy model and the newest
// drill record. Historical backup handoff exercises stay in
// security/handoff-exercises/ and are not evaluated.
import { createHash } from 'node:crypto';
import { existsSync, lstatSync, mkdirSync, readdirSync, readFileSync, realpathSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const workspaceRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const policyPath = path.join(workspaceRoot, '.ranvier-supply-chain-policy.json');
const allowedOutputRoot = path.join(workspaceRoot, 'target', 'supply-chain');
const drillDir = path.join(workspaceRoot, 'security', 'continuity-drills');
const EVENT_TYPES = ['acknowledged', 'initial_assessment', 'release_decision', 'drill_closed'];

function invariant(condition, message) {
  if (!condition) throw new Error(message);
}

function sha256(bytes) {
  return createHash('sha256').update(bytes).digest('hex');
}

function git(args) {
  const result = spawnSync('git', args, {
    cwd: workspaceRoot,
    encoding: 'utf8',
    windowsHide: true
  });
  invariant(result.status === 0, `git ${args.join(' ')} failed: ${result.stderr ?? ''}`);
  return result.stdout ?? '';
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

function substantive(value, min = 20) {
  return typeof value === 'string' && value.trim().length >= min;
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

function eventByType(drill, type) {
  const matches = (drill.events ?? []).filter((event) => event.type === type);
  invariant(matches.length === 1, `drill must contain exactly one ${type} event`);
  return matches[0];
}

export function validateSingleMaintainerPolicy(policy) {
  invariant(policy.schema_version === '2.0.0', 'unsupported policy schema');
  const ownership = policy.ownership ?? {};
  invariant(ownership.model === 'single-maintainer', 'ownership.model must be single-maintainer');
  invariant(substantive(ownership.primary, 2), 'ownership.primary is required');
  invariant(ownership.backup === null, 'single-maintainer policy must not name a backup');
  invariant(substantive(ownership.independent_review, 12), 'ownership.independent_review must describe the review');
  invariant(Number.isInteger(ownership.continuity_drill_max_age_days) && ownership.continuity_drill_max_age_days > 0,
    'ownership.continuity_drill_max_age_days must be a positive integer');
  invariant(Number.isFinite(ownership.acknowledgment_hours) && ownership.acknowledgment_hours > 0,
    'ownership.acknowledgment_hours must be positive');
}

export function evaluateDrill(policy, drill, now = new Date()) {
  const errors = [];
  const check = (condition, message) => {
    if (!condition) errors.push(message);
  };

  try {
    validateSingleMaintainerPolicy(policy);
    const primary = policy.ownership.primary;
    invariant(drill.schema_version === '2.0.0', 'unsupported drill schema');
    check(drill.drill_type === 'tabletop' && drill.synthetic === true, 'drill must be an explicit synthetic tabletop');
    check(drill.scenario?.severity === 'high', 'drill scenario must use a High advisory');
    check(/^RUSTSEC-2099-\d{4}$/.test(drill.scenario?.advisory_id ?? ''), 'drill advisory must be a reserved synthetic RUSTSEC-2099 id');
    check(drill.primary?.name === primary, 'drill primary does not match policy');
    check(drill.backup === undefined || drill.backup === null, 'single-maintainer drill must not name a backup');

    const detected = parseInstant(drill.scenario?.detected_at, 'scenario.detected_at');
    const started = parseInstant(drill.started_at, 'started_at');
    const completed = parseInstant(drill.completed_at, 'completed_at');
    check(started.getTime() >= detected.getTime(), 'drill starts before detection');
    check(completed.getTime() >= started.getTime(), 'drill completes before it starts');

    check(Array.isArray(drill.events) && drill.events.length === EVENT_TYPES.length, `drill must contain exactly ${EVENT_TYPES.length} events`);
    const events = EVENT_TYPES.map((type) => eventByType(drill, type));
    let previous = detected;
    for (const event of events) {
      check(event.actor === primary, `${event.type} must be performed by the primary`);
      check(substantive(event.response, 10), `${event.type} must preserve the primary's response`);
      const at = parseInstant(event.at, `${event.type}.at`);
      check(at.getTime() >= previous.getTime(), `${event.type} is out of order`);
      previous = at;
    }
    const [acknowledged, assessed, decision, closed] = events;
    const acknowledgmentHours = (parseInstant(acknowledged.at, 'acknowledged.at') - detected) / 3_600_000;
    check(acknowledgmentHours <= policy.ownership.acknowledgment_hours,
      `acknowledgment exceeds the ${policy.ownership.acknowledgment_hours} hour target`);
    check(assessed.severity === 'high', 'assessment must preserve High severity');
    check(decision.decision === 'no-go', 'High advisory drill must produce release no-go');
    check(substantive(decision.reason), 'release decision requires a rationale');
    check(substantive(closed.next_action), 'drill closure requires a next action');
    check(parseInstant(closed.at, 'drill_closed.at').getTime() === completed.getTime(), 'closure timeline is inconsistent');

    const review = drill.independent_review ?? {};
    check(review.reviewer === 'separate-session AI review', 'independent_review.reviewer must be a separate-session AI review');
    check(review.had_drill_conversation === false, 'independent reviewer must not have the drill conversation');
    check(substantive(review.session, 4), 'independent_review.session is required');
    const reviewedAt = parseInstant(review.reviewed_at, 'independent_review.reviewed_at');
    check(reviewedAt.getTime() >= completed.getTime(), 'independent review precedes drill completion');
    check(['pass', 'pass-with-findings'].includes(review.conclusion), 'independent review must conclude pass or pass-with-findings');
    check(Array.isArray(review.findings), 'independent_review.findings must be a list');
    check(review.primary_approval?.name === primary, 'primary approval of the review is required');
    const approvedAt = parseInstant(review.primary_approval?.at, 'independent_review.primary_approval.at');
    check(approvedAt.getTime() >= reviewedAt.getTime(), 'primary approval precedes the review');

    check(Array.isArray(drill.non_claims) && drill.non_claims.length >= 3, 'drill must preserve at least three non-claims');

    const ageDays = (now - completed) / 86_400_000;
    check(ageDays >= 0, 'drill completion is in the future');
    check(ageDays <= policy.ownership.continuity_drill_max_age_days,
      `drill is older than ${policy.ownership.continuity_drill_max_age_days} days`);
  } catch (error) {
    errors.push(error.message);
  }

  return { passed: errors.length === 0, errors };
}

export function syntheticFixture(policy) {
  const primary = policy.ownership.primary;
  const event = (type, at, extra) => ({ type, actor: primary, at, response: `synthetic ${type} response text`, ...extra });
  return {
    schema_version: '2.0.0',
    drill_id: 'self-test',
    drill_type: 'tabletop',
    synthetic: true,
    started_at: '2026-11-01T00:00:00Z',
    completed_at: '2026-11-01T00:30:00Z',
    scenario: {
      advisory_id: 'RUSTSEC-2099-0001',
      package: 'synthetic-dependency',
      version: '1.0.0',
      severity: 'high',
      detected_at: '2026-11-01T00:00:00Z',
      description: 'Synthetic High advisory for the gate self-test.'
    },
    primary: { name: primary },
    events: [
      event('acknowledged', '2026-11-01T00:05:00Z'),
      event('initial_assessment', '2026-11-01T00:10:00Z', { severity: 'high' }),
      event('release_decision', '2026-11-01T00:20:00Z', { decision: 'no-go', reason: 'High advisories block publication until fixed.' }),
      event('drill_closed', '2026-11-01T00:30:00Z', { next_action: 'Upgrade the dependency and rerun the locked gate.' })
    ],
    independent_review: {
      reviewer: 'separate-session AI review',
      session: 'self-test',
      had_drill_conversation: false,
      reviewed_at: '2026-11-01T01:00:00Z',
      conclusion: 'pass',
      findings: [],
      primary_approval: { name: primary, at: '2026-11-01T01:10:00Z' }
    },
    non_claims: ['not a mailbox test', 'not a permissions test', 'not a real incident']
  };
}

function runSelfTest(policy) {
  const now = new Date('2026-11-02T00:00:00Z');
  const fixture = syntheticFixture(policy);
  const valid = evaluateDrill(policy, fixture, now);
  invariant(valid.passed, `valid drill fixture did not pass: ${valid.errors.join('; ')}`);
  const expectFail = (label, mutate, at = now) => {
    const drill = structuredClone(fixture);
    const pol = structuredClone(policy);
    mutate(drill, pol);
    invariant(!evaluateDrill(pol, drill, at).passed, `${label} fixture unexpectedly passed`);
  };
  expectFail('named backup', (_, pol) => { pol.ownership.backup = 'Someone Else'; });
  expectFail('team model', (_, pol) => { pol.ownership.model = 'primary-backup'; });
  expectFail('drill backup', (drill) => { drill.backup = { name: 'Someone Else' }; });
  expectFail('other actor', (drill) => { eventByType(drill, 'release_decision').actor = 'Someone Else'; });
  expectFail('go decision', (drill) => { eventByType(drill, 'release_decision').decision = 'go'; });
  expectFail('missing review', (drill) => { delete drill.independent_review; });
  expectFail('review with conversation', (drill) => { drill.independent_review.had_drill_conversation = true; });
  expectFail('review before drill', (drill) => { drill.independent_review.reviewed_at = '2026-11-01T00:15:00Z'; });
  expectFail('failed review', (drill) => { drill.independent_review.conclusion = 'fail'; });
  expectFail('approval before review', (drill) => { drill.independent_review.primary_approval.at = '2026-11-01T00:45:00Z'; });
  expectFail('late acknowledgment', (drill) => {
    eventByType(drill, 'acknowledged').at = '2026-11-03T01:00:00Z';
  }, new Date('2026-11-04T00:00:00Z'));
  expectFail('stale drill', () => {}, new Date('2027-03-01T00:00:00Z'));
  expectFail('invalid date', (drill) => { drill.started_at = '2026-02-30T15:30:00Z'; });
  console.log('[maintenance-continuity] negative self-test passed');
}

function latestDrill() {
  invariant(existsSync(drillDir), `missing drill directory: ${path.relative(workspaceRoot, drillDir)}`);
  const files = readdirSync(drillDir).filter((name) => name.endsWith('.json')).sort();
  invariant(files.length > 0, 'no continuity drill record found');
  return path.join(drillDir, files[files.length - 1]);
}

function parseArgs(argv) {
  const args = { selfTest: false, drill: null, output: null };
  for (let i = 0; i < argv.length; i += 1) {
    if (argv[i] === '--self-test') args.selfTest = true;
    else if (argv[i] === '--drill' || argv[i] === '--exercise') args.drill = path.resolve(workspaceRoot, argv[++i]);
    else if (argv[i] === '--output') args.output = path.resolve(workspaceRoot, argv[++i]);
    else throw new Error(`unknown argument: ${argv[i]}`);
  }
  return args;
}

function invokedDirectly() {
  if (!process.argv[1]) return false;
  const norm = (value) => (process.platform === 'win32' ? value.toLowerCase() : value);
  return norm(path.resolve(process.argv[1])) === norm(fileURLToPath(import.meta.url));
}

if (invokedDirectly()) {
  const args = parseArgs(process.argv.slice(2));
  const policyBytes = readFileSync(policyPath);
  const policy = JSON.parse(policyBytes);
  if (args.selfTest) runSelfTest(policy);
  const drillPath = args.drill ?? latestDrill();
  const drillBytes = readFileSync(drillPath);
  const drill = JSON.parse(drillBytes);
  const result = {
    schema_version: '2.0.0',
    evaluated_at: new Date().toISOString(),
    source: {
      commit: git(['rev-parse', 'HEAD']).trim(),
      tree_clean: git(['status', '--porcelain=v1', '--untracked-files=all']).trim() === ''
    },
    policy: path.relative(workspaceRoot, policyPath).replaceAll('\\', '/'),
    policy_sha256: sha256(policyBytes),
    drill: path.relative(workspaceRoot, drillPath).replaceAll('\\', '/'),
    drill_sha256: sha256(drillBytes),
    ...evaluateDrill(policy, drill)
  };
  if (args.output) {
    const output = safeOutputPath(args.output);
    mkdirSync(path.dirname(output), { recursive: true });
    writeFileSync(output, `${JSON.stringify(result, null, 2)}\n`, 'utf8');
  }
  if (!result.passed) {
    console.error(`[maintenance-continuity] FAILED\n${result.errors.map((error) => `- ${error}`).join('\n')}`);
    process.exitCode = 1;
  } else {
    console.log(`[maintenance-continuity] passed: ${drill.drill_id}`);
  }
}
