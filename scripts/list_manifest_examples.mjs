#!/usr/bin/env node

import { readFile, stat } from "node:fs/promises";
import { spawnSync } from "node:child_process";
import path from "node:path";
import process from "node:process";

const DEFAULT_MANIFEST = ".ranvier-examples-manifest.json";

function parseArgs(argv) {
  const options = {
    manifest: DEFAULT_MANIFEST,
    tiers: null,
    runtime: "any",
    field: "package",
    format: "lines",
    verifyWorkspaceMembers: false,
  };

  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    const next = () => {
      i += 1;
      if (i >= argv.length) {
        throw new Error(`Missing value for ${arg}`);
      }
      return argv[i];
    };

    switch (arg) {
      case "--manifest":
        options.manifest = next();
        break;
      case "--tiers":
        options.tiers = next()
          .split(",")
          .map((tier) => tier.trim())
          .filter(Boolean);
        break;
      case "--runtime":
        options.runtime = next();
        break;
      case "--field":
        options.field = next();
        break;
      case "--format":
        options.format = next();
        break;
      case "--verify-workspace-members":
        options.verifyWorkspaceMembers = true;
        break;
      case "--help":
      case "-h":
        printHelp();
        process.exit(0);
        break;
      default:
        throw new Error(`Unknown argument: ${arg}`);
    }
  }

  if (!["any", "none"].includes(options.runtime)) {
    throw new Error("--runtime must be one of: any, none");
  }
  if (!["id", "package"].includes(options.field)) {
    throw new Error("--field must be one of: id, package");
  }
  if (!["lines", "json"].includes(options.format)) {
    throw new Error("--format must be one of: lines, json");
  }

  return options;
}

function printHelp() {
  console.log(`Usage: node scripts/list_manifest_examples.mjs [options]

Options:
  --manifest <path>                Manifest path (default: .ranvier-examples-manifest.json)
  --tiers <csv>                    Include only these tiers, e.g. core or core,lab
  --runtime <any|none>             Include any examples or only examples without runtimeRequirements
  --field <id|package>             Output field (default: package)
  --format <lines|json>            Output format (default: lines)
  --verify-workspace-members       Fail if selected packages are not Cargo workspace members
`);
}

async function pathExists(candidate) {
  try {
    await stat(candidate);
    return true;
  } catch {
    return false;
  }
}

async function loadManifest(manifestPath) {
  const resolved = path.resolve(process.cwd(), manifestPath);
  const raw = await readFile(resolved, "utf8");
  const manifest = JSON.parse(raw);
  if (!Array.isArray(manifest.examples)) {
    throw new Error(`Manifest does not contain an examples array: ${resolved}`);
  }
  return { manifest, resolved };
}

function filterExamples(examples, options) {
  return examples.filter((entry) => {
    if (!entry || typeof entry !== "object") {
      return false;
    }
    if (options.tiers && !options.tiers.includes(entry.tier)) {
      return false;
    }
    if (options.runtime === "none") {
      const requirements = Array.isArray(entry.runtimeRequirements)
        ? entry.runtimeRequirements
        : [];
      if (requirements.length > 0) {
        return false;
      }
    }
    return true;
  });
}

function getWorkspacePackageNames() {
  const metadata = spawnSync(
    "cargo",
    ["metadata", "--format-version", "1", "--no-deps", "--manifest-path", "Cargo.toml"],
    {
      cwd: process.cwd(),
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    }
  );

  if (metadata.status !== 0) {
    const stderr = metadata.stderr.trim();
    throw new Error(`cargo metadata failed: ${stderr || metadata.stdout.trim()}`);
  }

  const parsed = JSON.parse(metadata.stdout);
  const workspaceMembers = new Set(parsed.workspace_members);
  return new Set(
    parsed.packages
      .filter((pkg) => workspaceMembers.has(pkg.id))
      .map((pkg) => pkg.name)
  );
}

async function verifyExamples(entries) {
  const workspacePackages = getWorkspacePackageNames();
  const errors = [];

  for (const entry of entries) {
    const packageName = entry.package;
    if (!workspacePackages.has(packageName)) {
      errors.push(`${packageName}: not found in Cargo workspace members`);
    }

    const manifestPath = path.join(process.cwd(), "examples", packageName, "Cargo.toml");
    if (!(await pathExists(manifestPath))) {
      errors.push(`${packageName}: missing examples/${packageName}/Cargo.toml`);
    }
  }

  if (errors.length > 0) {
    for (const error of errors) {
      console.error(`[examples-manifest] ${error}`);
    }
    throw new Error(`${errors.length} selected example workspace mismatch(es)`);
  }
}

function printEntries(entries, options) {
  const values = entries.map((entry) => entry[options.field]);
  if (options.format === "json") {
    console.log(JSON.stringify(values, null, 2));
    return;
  }

  for (const value of values) {
    console.log(value);
  }
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  const { manifest } = await loadManifest(options.manifest);
  const entries = filterExamples(manifest.examples, options);

  if (options.verifyWorkspaceMembers) {
    await verifyExamples(entries);
  }

  printEntries(entries, options);
}

main().catch((error) => {
  console.error(`[examples-manifest] ${error.message}`);
  process.exitCode = 1;
});
