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
    supportTiers: null,
    runtime: "any",
    field: "package",
    format: "lines",
    verifyWorkspaceMembers: false,
    verifyPortfolio: false,
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
      case "--support-tiers":
        options.supportTiers = next()
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
      case "--verify-portfolio":
        options.verifyPortfolio = true;
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
  --support-tiers <csv>            Include only these support tiers, e.g. canonical,supported
  --runtime <any|none>             Include any examples or only examples without runtimeRequirements
  --field <id|package>             Output field (default: package)
  --format <lines|json>            Output format (default: lines)
  --verify-workspace-members       Fail if selected packages are not Cargo workspace members
  --verify-portfolio               Fail if manifest support tier and owner metadata is incomplete
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
    if (
      options.supportTiers &&
      !options.supportTiers.includes(entry.supportTier)
    ) {
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

function manifestCargoPath(entry) {
  const examplePath =
    typeof entry.path === "string" && entry.path.trim() !== ""
      ? entry.path
      : path.join("examples", entry.package);
  return path.join(process.cwd(), examplePath, "Cargo.toml");
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

function verifyPortfolio(manifest) {
  const supportTiers = manifest.supportTiers ?? {};
  const validSupportTiers = new Set([
    "canonical",
    "supported",
    "lab",
    "archive",
  ]);
  const errors = [];
  const counts = new Map();
  const expectedCiGate = new Map([
    ["canonical", "developer"],
    ["supported", "release"],
    ["lab", "scheduled-lab"],
    ["archive", "excluded"],
  ]);
  const entriesById = new Map();

  if (manifest.portfolioPolicy?.maxCanonical !== 5) {
    errors.push("portfolioPolicy.maxCanonical must be 5");
  }
  if (manifest.portfolioPolicy?.maxSupported !== 12) {
    errors.push("portfolioPolicy.maxSupported must be 12");
  }

  for (const tier of validSupportTiers) {
    if (typeof supportTiers[tier] !== "string" || supportTiers[tier].trim() === "") {
      errors.push(`missing supportTiers.${tier} description`);
    }
  }

  for (const entry of manifest.examples) {
    if (entriesById.has(entry.id)) {
      errors.push(`${entry.id}: duplicate example id`);
    }
    entriesById.set(entry.id, entry);

    if (!validSupportTiers.has(entry.supportTier)) {
      errors.push(`${entry.id ?? entry.package}: invalid or missing supportTier`);
    } else {
      counts.set(entry.supportTier, (counts.get(entry.supportTier) ?? 0) + 1);
    }

    if (typeof entry.owner !== "string" || entry.owner.trim() === "") {
      errors.push(`${entry.id ?? entry.package}: missing owner`);
    }

    if (
      !Array.isArray(entry.runtimeRequirements) ||
      entry.runtimeRequirements.some(
        (item) => typeof item !== "string" || item.trim() === ""
      )
    ) {
      errors.push(`${entry.id ?? entry.package}: runtimeRequirements must be an array of non-empty strings`);
    }

    if (entry.ciGate !== expectedCiGate.get(entry.supportTier)) {
      errors.push(
        `${entry.id}: ${entry.supportTier} examples must use ` +
          `ciGate=${expectedCiGate.get(entry.supportTier)}`
      );
    }

    if (["canonical", "supported"].includes(entry.supportTier)) {
      if (
        typeof entry.manualLink !== "string" ||
        !/^https:\/\/ranvier\.dev\/docs\//.test(entry.manualLink)
      ) {
        errors.push(`${entry.id}: ${entry.supportTier} examples require a ranvier.dev manualLink`);
      }
      if (
        entry.manualLink?.includes("/docs/examples-interactive#") &&
        !entry.manualLink.endsWith(`#example-${entry.id}`)
      ) {
        errors.push(`${entry.id}: interactive manualLink must use its stable example anchor`);
      }
      if (typeof entry.supportRationale !== "string" || entry.supportRationale.trim() === "") {
        errors.push(`${entry.id}: ${entry.supportTier} examples require supportRationale`);
      }
    }

    if (entry.supportTier === "canonical" && entry.tier !== "core") {
      errors.push(`${entry.id}: canonical examples must remain in web tier core`);
    }
    if (entry.supportTier === "supported" && entry.tier !== "core") {
      errors.push(`${entry.id}: supported examples must remain in web tier core`);
    }
    if (entry.supportTier === "lab" && entry.tier !== "lab") {
      errors.push(`${entry.id}: lab examples must use web tier lab`);
    }
    if (entry.supportTier === "archive" && entry.tier !== "repo") {
      errors.push(`${entry.id}: archive examples must use web tier repo`);
    }
    if (entry.tier === "repo" && entry.supportTier !== "archive") {
      errors.push(`${entry.id}: repo-tier examples must use supportTier archive`);
    }
    if (
      entry.supportTier === "archive" &&
      (typeof entry.path !== "string" ||
        !entry.path.startsWith("examples/experimental/"))
    ) {
      errors.push(`${entry.id}: archive examples must declare examples/experimental path`);
    }
  }

  for (const required of ["canonical", "supported", "lab", "archive"]) {
    if ((counts.get(required) ?? 0) === 0) {
      errors.push(`support tier has no examples: ${required}`);
    }
  }

  if ((counts.get("canonical") ?? 0) > 5) {
    errors.push(`canonical example cap exceeded: ${counts.get("canonical")}/5`);
  }
  if ((counts.get("supported") ?? 0) > 12) {
    errors.push(`supported example cap exceeded: ${counts.get("supported")}/12`);
  }

  for (const learningPath of manifest.learningPaths ?? []) {
    for (const step of learningPath.steps ?? []) {
      const entry = entriesById.get(step);
      if (!entry) {
        errors.push(`learning path ${learningPath.id} references missing example ${step}`);
      } else if (!["canonical", "supported"].includes(entry.supportTier)) {
        errors.push(`learning path ${learningPath.id} references non-maintained example ${step}`);
      }
    }
  }

  if (errors.length > 0) {
    for (const error of errors) {
      console.error(`[examples-manifest] ${error}`);
    }
    throw new Error(`${errors.length} example portfolio metadata error(s)`);
  }
}

async function verifyCatalogMirror(manifest) {
  const catalogPath = path.join(process.cwd(), "examples", "catalog.json");
  const errors = [];

  if (!(await pathExists(catalogPath))) {
    throw new Error(`missing example compatibility catalog: ${catalogPath}`);
  }

  const catalog = JSON.parse(await readFile(catalogPath, "utf8"));
  if (!Array.isArray(catalog.entries)) {
    throw new Error("examples/catalog.json does not contain an entries array");
  }

  const manifestByCatalogName = new Map();
  for (const entry of manifest.examples) {
    manifestByCatalogName.set(entry.package, entry);
    if (
      typeof entry.path === "string" &&
      entry.path.startsWith("examples/experimental/")
    ) {
      manifestByCatalogName.set(entry.path.replace(/^examples\//, ""), entry);
    }
  }

  const catalogNames = new Set();
  for (const entry of catalog.entries) {
    catalogNames.add(entry.name);
    const manifestEntry = manifestByCatalogName.get(entry.name);
    if (!manifestEntry) {
      errors.push(`catalog entry missing from manifest: ${entry.name}`);
      continue;
    }
    if (entry.support_tier !== manifestEntry.supportTier) {
      errors.push(
        `${entry.name}: catalog support_tier=${entry.support_tier ?? "-"} manifest supportTier=${manifestEntry.supportTier}`
      );
    }
    if (entry.owner !== manifestEntry.owner) {
      errors.push(
        `${entry.name}: catalog owner=${entry.owner ?? "-"} manifest owner=${manifestEntry.owner}`
      );
    }
  }

  for (const entry of manifest.examples) {
    const catalogName =
      typeof entry.path === "string" &&
      entry.path.startsWith("examples/experimental/")
        ? entry.path.replace(/^examples\//, "")
        : entry.package;
    if (!catalogNames.has(catalogName)) {
      errors.push(`manifest entry missing from catalog: ${catalogName}`);
    }
  }

  if (errors.length > 0) {
    for (const error of errors) {
      console.error(`[examples-manifest] ${error}`);
    }
    throw new Error(`${errors.length} example catalog mirror error(s)`);
  }
}

async function verifyExamples(entries) {
  const workspacePackages = getWorkspacePackageNames();
  const errors = [];

  for (const entry of entries) {
    const packageName = entry.package;
    if (!workspacePackages.has(packageName)) {
      errors.push(`${packageName}: not found in Cargo workspace members`);
    }

    const manifestPath = manifestCargoPath(entry);
    if (!(await pathExists(manifestPath))) {
      errors.push(`${packageName}: missing ${path.relative(process.cwd(), manifestPath)}`);
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
  if (options.verifyPortfolio) {
    verifyPortfolio(manifest);
    await verifyCatalogMirror(manifest);
  }
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
