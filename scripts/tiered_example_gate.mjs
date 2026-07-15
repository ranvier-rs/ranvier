#!/usr/bin/env node

import { mkdir, readFile, writeFile } from "node:fs/promises";
import { spawnSync } from "node:child_process";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const workspaceRoot = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "..",
);
const evidenceRoot = path.join(workspaceRoot, "target", "tier-gate-evidence");
const laneAliases = new Map([
  ["developer", "developer"],
  ["release", "release"],
  ["lab", "scheduled-lab"],
]);
const validPhases = new Set(["plan", "check", "test", "clippy", "node", "all"]);

function parseArgs(argv) {
  const options = { lane: null, phase: "all" };
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    const next = () => {
      index += 1;
      if (index >= argv.length) throw new Error(`Missing value for ${arg}`);
      return argv[index];
    };
    if (arg === "--lane") options.lane = next();
    else if (arg === "--phase") options.phase = next();
    else if (arg === "--help" || arg === "-h") {
      console.log(
        "Usage: node scripts/tiered_example_gate.mjs --lane <developer|release|lab> " +
          "[--phase <plan|check|test|clippy|node|all>]",
      );
      process.exit(0);
    } else throw new Error(`Unknown argument: ${arg}`);
  }
  if (!laneAliases.has(options.lane)) {
    throw new Error("--lane must be one of: developer, release, lab");
  }
  if (!validPhases.has(options.phase)) {
    throw new Error(
      "--phase must be one of: plan, check, test, clippy, node, all",
    );
  }
  return options;
}

function readJson(file) {
  return readFile(file, "utf8").then((value) =>
    JSON.parse(value.replace(/^\uFEFF/, "")),
  );
}

function executable(name) {
  if (process.platform !== "win32") return name;
  return `${name}.exe`;
}

function invocation(command, args) {
  if (process.platform === "win32" && command === "npm") {
    return {
      executable: process.env.ComSpec ?? "cmd.exe",
      args: ["/d", "/s", "/c", "npm.cmd", ...args],
    };
  }
  return { executable: executable(command), args };
}

function capture(command, args) {
  const call = invocation(command, args);
  const result = spawnSync(call.executable, call.args, {
    cwd: workspaceRoot,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
  if ((result.status ?? 1) !== 0) {
    throw new Error(
      `${command} ${args.join(" ")} failed: ` +
        `${result.error?.message || result.stderr?.trim() || result.stdout?.trim() || "unknown error"}`,
    );
  }
  return result.stdout.trim();
}

function packageArgs(packages) {
  return packages.flatMap((packageName) => ["-p", packageName]);
}

function sameSet(left, right) {
  return (
    left.size === right.size && [...left].every((value) => right.has(value))
  );
}

function resolvePlan(manifest, apiPolicy, metadata, requestedLane) {
  const manifestGate = laneAliases.get(requestedLane);
  const packagesById = new Map(
    metadata.packages.map((entry) => [entry.id, entry]),
  );
  const workspacePackages = new Set(
    metadata.workspace_members
      .map((id) => packagesById.get(id)?.name)
      .filter(Boolean),
  );
  const defaultPackages = new Set(
    metadata.workspace_default_members
      .map((id) => packagesById.get(id)?.name)
      .filter(Boolean),
  );
  const productPackages = Object.keys(apiPolicy.crates).sort();
  const entries = manifest.examples.filter(
    (entry) => entry.ciGate === manifestGate,
  );
  const archiveEntries = manifest.examples.filter(
    (entry) => entry.ciGate === "excluded",
  );
  const canonicalPackages = manifest.examples
    .filter((entry) => entry.ciGate === "developer")
    .map((entry) => entry.package)
    .sort();
  const expectedDefault = new Set([...productPackages, ...canonicalPackages]);

  for (const packageName of [
    ...productPackages,
    ...manifest.examples.map((entry) => entry.package),
  ]) {
    if (!workspacePackages.has(packageName)) {
      throw new Error(
        `Manifest or product package is not a workspace member: ${packageName}`,
      );
    }
  }
  if (!sameSet(defaultPackages, expectedDefault)) {
    const missing = [...expectedDefault].filter(
      (name) => !defaultPackages.has(name),
    );
    const extra = [...defaultPackages].filter(
      (name) => !expectedDefault.has(name),
    );
    throw new Error(
      `workspace default-members drift; missing=${missing.join(",") || "none"} ` +
        `extra=${extra.join(",") || "none"}`,
    );
  }
  if (archiveEntries.length === 0)
    throw new Error("Archive/excluded tier is empty");
  if (entries.some((entry) => entry.supportTier === "archive")) {
    throw new Error("Archive entries cannot be selected by an executable lane");
  }
  if (
    requestedLane === "developer" &&
    entries.some((entry) => entry.runtimeRequirements.length)
  ) {
    throw new Error(
      "Canonical developer examples must not require external runtimes",
    );
  }

  const examplePackages = entries.map((entry) => entry.package).sort();
  const cargoPackages =
    requestedLane === "developer"
      ? [...new Set([...productPackages, ...examplePackages])].sort()
      : examplePackages;
  const testPackages =
    requestedLane === "lab"
      ? []
      : requestedLane === "developer"
        ? cargoPackages
        : entries
            .filter((entry) => entry.runtimeRequirements.length === 0)
            .map((entry) => entry.package)
            .sort();
  const nodeProjects = entries
    .filter((entry) => entry.runtimeRequirements.includes("nodejs"))
    .map((entry) => ({
      example: entry.id,
      directory: path.join(
        workspaceRoot,
        entry.path ?? path.join("examples", entry.package),
        "frontend",
      ),
    }));
  const policy = manifest.gatePolicy?.[manifestGate];
  if (!policy) throw new Error(`Missing gatePolicy.${manifestGate}`);
  if (
    requestedLane === "release" &&
    nodeProjects.length > 0 &&
    Number(process.versions.node.split(".")[0]) !== policy.nodeMajor
  ) {
    throw new Error(
      `Release Node projects require Node ${policy.nodeMajor}; current ${process.versions.node}`,
    );
  }

  return {
    requestedLane,
    manifestGate,
    policy,
    productPackages,
    examplePackages,
    cargoPackages,
    testPackages,
    nodeProjects,
    runtimeDeferred: entries
      .filter((entry) => entry.runtimeRequirements.length > 0)
      .map((entry) => ({
        id: entry.id,
        requirements: entry.runtimeRequirements,
      })),
    archivePackages: archiveEntries.map((entry) => entry.package).sort(),
  };
}

function commandPlan(plan, phase) {
  const cargo = (label, subcommand, packages, trailing = []) => ({
    label,
    command: "cargo",
    args: [subcommand, "--locked", ...packageArgs(packages), ...trailing],
    cwd: workspaceRoot,
  });
  const commands = [];
  const allPhases = {
    developer: ["check", "test", "clippy"],
    release: ["check", "test", "node"],
    lab: ["check"],
  };
  const selectedPhases = new Set(
    phase === "all" ? allPhases[plan.requestedLane] : [phase],
  );
  const include = (candidate) => selectedPhases.has(candidate);
  const allowedPhases = new Set([
    "plan",
    "all",
    ...allPhases[plan.requestedLane],
  ]);
  if (!allowedPhases.has(phase)) {
    throw new Error(`${phase} is not part of the ${plan.requestedLane} lane`);
  }

  if (include("check")) {
    const trailing =
      plan.requestedLane === "developer" ? ["--features", "streaming"] : [];
    commands.push(
      cargo(
        `${plan.requestedLane}-cargo-check`,
        "check",
        plan.cargoPackages,
        trailing,
      ),
    );
  }
  if (include("test") && plan.testPackages.length > 0) {
    commands.push(
      cargo(`${plan.requestedLane}-cargo-test`, "test", plan.testPackages),
    );
  }
  if (include("clippy")) {
    if (plan.requestedLane !== "developer") {
      throw new Error("Clippy is only part of the developer lane");
    }
    commands.push(
      cargo("developer-cargo-clippy", "clippy", plan.cargoPackages, [
        "--features",
        "streaming",
        "--",
        "-D",
        "warnings",
      ]),
    );
  }
  if (include("node")) {
    if (plan.requestedLane !== "release") {
      throw new Error("Node validation is only part of the release lane");
    }
    for (const project of plan.nodeProjects) {
      commands.push(
        {
          label: `${project.example}-npm-ci`,
          command: "npm",
          args: ["ci"],
          cwd: project.directory,
        },
        {
          label: `${project.example}-npm-check`,
          command: "npm",
          args: ["run", "check"],
          cwd: project.directory,
        },
        {
          label: `${project.example}-npm-build`,
          command: "npm",
          args: ["run", "build"],
          cwd: project.directory,
        },
      );
    }
  }
  return commands;
}

async function writeEvidence(evidence) {
  await mkdir(evidenceRoot, { recursive: true });
  const output = path.join(evidenceRoot, `${evidence.lane}.json`);
  await writeFile(output, `${JSON.stringify(evidence, null, 2)}\n`, "utf8");
  console.log(`[tier-gate] evidence: ${path.relative(workspaceRoot, output)}`);
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  capture("node", [
    "scripts/list_manifest_examples.mjs",
    "--verify-portfolio",
    "--verify-workspace-members",
    "--format",
    "json",
  ]);
  const [manifest, apiPolicy] = await Promise.all([
    readJson(path.join(workspaceRoot, ".ranvier-examples-manifest.json")),
    readJson(path.join(workspaceRoot, ".ranvier-api-policy.json")),
  ]);
  const metadata = JSON.parse(
    capture("cargo", [
      "metadata",
      "--format-version",
      "1",
      "--no-deps",
      "--locked",
    ]),
  );
  const plan = resolvePlan(manifest, apiPolicy, metadata, options.lane);
  const commands =
    options.phase === "plan" ? [] : commandPlan(plan, options.phase);
  const startedAt = new Date();
  const deadline = startedAt.getTime() + plan.policy.timeoutMinutes * 60_000;
  const evidence = {
    schemaVersion: "1.0.0",
    lane: options.lane,
    manifestGate: plan.manifestGate,
    phase: options.phase,
    status: commands.length === 0 ? "planned" : "running",
    startedAt: startedAt.toISOString(),
    finishedAt: null,
    durationSeconds: null,
    policy: plan.policy,
    sourceCommit: capture("git", ["rev-parse", "HEAD"]),
    toolchain: {
      rustc: capture("rustc", ["--version"]),
      cargo: capture("cargo", ["--version"]),
      node: process.version,
      npm: plan.nodeProjects.length > 0 ? capture("npm", ["--version"]) : null,
    },
    selection: {
      productPackages: plan.productPackages,
      examplePackages: plan.examplePackages,
      cargoPackages: plan.cargoPackages,
      testPackages: plan.testPackages,
      runtimeDeferred: plan.runtimeDeferred,
      archivePackagesExcluded: plan.archivePackages,
    },
    commands: [],
  };

  console.log(
    `[tier-gate] ${options.lane}/${options.phase}: ` +
      `${plan.productPackages.length} product, ${plan.examplePackages.length} example, ` +
      `${plan.testPackages.length} test package(s)`,
  );
  try {
    for (const command of commands) {
      const commandStarted = Date.now();
      console.log(`[tier-gate] ${command.label}`);
      const call = invocation(command.command, command.args);
      const remainingMilliseconds = deadline - Date.now();
      if (remainingMilliseconds <= 0) {
        throw new Error(
          `${options.lane} lane exceeded its ${plan.policy.timeoutMinutes}-minute budget`,
        );
      }
      const result = spawnSync(call.executable, call.args, {
        cwd: command.cwd,
        env: process.env,
        stdio: "inherit",
        timeout: remainingMilliseconds,
      });
      const record = {
        label: command.label,
        command: `${command.command} ${command.args.join(" ")}`,
        directory:
          path.relative(workspaceRoot, command.cwd).replaceAll("\\", "/") ||
          ".",
        durationSeconds: Number(
          ((Date.now() - commandStarted) / 1000).toFixed(3),
        ),
        exitCode: result.status,
        timedOut: result.error?.code === "ETIMEDOUT",
      };
      evidence.commands.push(record);
      if (result.error) {
        throw new Error(`${command.label} failed: ${result.error.message}`);
      }
      if ((result.status ?? 1) !== 0) {
        throw new Error(
          `${command.label} failed with exit code ${result.status}`,
        );
      }
    }
    evidence.status = commands.length === 0 ? "planned" : "passed";
  } catch (error) {
    evidence.status = "failed";
    evidence.error = error.message;
    throw error;
  } finally {
    const finishedAt = new Date();
    evidence.finishedAt = finishedAt.toISOString();
    evidence.durationSeconds = Number(
      ((finishedAt - startedAt) / 1000).toFixed(3),
    );
    await writeEvidence(evidence);
  }
}

main().catch((error) => {
  console.error(`[tier-gate] ${error.message}`);
  process.exitCode = 1;
});
