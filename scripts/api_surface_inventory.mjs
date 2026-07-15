#!/usr/bin/env node
import { createHash } from 'node:crypto';
import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const workspaceRoot = path.resolve(scriptDir, '..');
const policyPath = path.join(workspaceRoot, '.ranvier-api-policy.json');
const outputPath = path.join(workspaceRoot, 'api-surface-inventory.json');
const mode = process.argv[2] ?? '--check';

if (!['--write', '--check'].includes(mode)) {
  console.error('usage: node scripts/api_surface_inventory.mjs [--write|--check]');
  process.exit(2);
}

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: options.cwd ?? workspaceRoot,
    env: options.env ?? process.env,
    encoding: 'utf8',
    maxBuffer: 64 * 1024 * 1024,
    stdio: options.inherit ? 'inherit' : 'pipe'
  });
  if ((result.status ?? 1) !== 0) {
    const detail = result.stderr?.trim() || result.stdout?.trim() || `exit ${result.status}`;
    throw new Error(`${command} ${args.join(' ')} failed: ${detail}`);
  }
  return result.stdout ?? '';
}

function canonicalText(value) {
  return value.toString('utf8').replace(/^\uFEFF/, '').replace(/\r\n?/g, '\n');
}

function sha256(value) {
  return createHash('sha256').update(value).digest('hex');
}

function readJson(file) {
  return JSON.parse(canonicalText(readFileSync(file)));
}

function relative(file) {
  return path.relative(workspaceRoot, file).replaceAll('\\', '/');
}

function itemKind(item) {
  return Object.keys(item?.inner ?? {})[0] ?? 'unknown';
}

function sourceLocation(item) {
  if (!item?.span) return null;
  return { file: item.span.filename.replaceAll('\\', '/') };
}

function matchesPrefix(itemPath, prefix) {
  return itemPath === prefix || itemPath.startsWith(`${prefix}::`);
}

function matchesRule(itemPath, rule) {
  return rule.match === 'exact' ? itemPath === rule.prefix : matchesPrefix(itemPath, rule.prefix);
}

function classify(policy, itemPath, rustdocDeprecation) {
  const rules = [...(policy.rules ?? [])].sort((a, b) => b.prefix.length - a.prefix.length);
  const boundaryRules = [...(policy.boundaries ?? [])].sort((a, b) => b.prefix.length - a.prefix.length);
  const deprecationRules = [...(policy.deprecations ?? [])].sort((a, b) => b.prefix.length - a.prefix.length);
  const rule = rules.find((candidate) => matchesRule(itemPath, candidate));
  const boundary = boundaryRules.find((candidate) => matchesPrefix(itemPath, candidate.prefix));
  const deprecation = deprecationRules.find((candidate) => matchesPrefix(itemPath, candidate.prefix));
  if (rustdocDeprecation && !deprecation) {
    throw new Error(`deprecated public item has no migration policy: ${itemPath}`);
  }
  return {
    stability: rustdocDeprecation || deprecation ? 'Deprecated' : rule?.tier ?? policy.default_tier,
    owner: rule?.owner ?? policy.owner,
    boundary: boundary?.kind ?? null,
    deprecation
  };
}

function addItem(items, seen, cratePolicy, raw) {
  if (!raw.path || !raw.kind) return;
  const key = `${raw.path}\u0000${raw.kind}\u0000${raw.origin ?? 'direct'}`;
  if (seen.has(key)) return;
  seen.add(key);
  const classification = classify(cratePolicy, raw.path, raw.deprecated);
  const deprecated = Boolean(raw.deprecated || classification.deprecation);
  items.push({
    path: raw.path,
    kind: raw.kind,
    origin: raw.origin ?? 'direct',
    target: raw.target ?? null,
    stability: classification.stability,
    owner: classification.owner,
    boundary: classification.boundary,
    deprecated,
    deprecation: deprecated
      ? {
          since: raw.deprecated?.since ?? null,
          note: raw.deprecated?.note ?? null,
          replacement: classification.deprecation.replacement,
          removal_condition: classification.deprecation.removal_condition,
          earliest_removal: classification.deprecation.earliest_removal
        }
      : null,
    source: raw.source ?? null
  });
}

function collectAssociatedItems(json, canonicalPaths, cratePolicy, items, seen) {
  for (const [id, descriptor] of canonicalPaths) {
    const item = json.index[id];
    if (!item) continue;
    const basePath = descriptor.path.join('::');
    const inner = item.inner ?? {};
    const childIds = [];

    if (inner.struct) {
      const kind = inner.struct.kind ?? {};
      childIds.push(...(kind.plain?.fields ?? []), ...(kind.tuple ?? []));
    }
    if (inner.union) childIds.push(...(inner.union.fields ?? []));
    if (inner.trait) childIds.push(...(inner.trait.items ?? []));

    for (const childId of childIds) {
      const child = json.index[childId];
      if (!child) continue;
      const publicTraitMember = Boolean(inner.trait);
      if (child.visibility !== 'public' && !publicTraitMember) continue;
      const name = child.name ?? `#${childId}`;
      addItem(items, seen, cratePolicy, {
        path: `${basePath}::${name}`,
        kind: itemKind(child),
        deprecated: child.deprecation,
        source: sourceLocation(child)
      });
    }

    const implIds = inner.struct?.impls ?? inner.enum?.impls ?? inner.union?.impls ?? [];
    for (const implId of implIds) {
      const implItem = json.index[implId]?.inner?.impl;
      if (!implItem || implItem.trait || implItem.is_synthetic || implItem.blanket_impl) continue;
      for (const associatedId of implItem.items ?? []) {
        const associated = json.index[associatedId];
        if (!associated || associated.visibility !== 'public') continue;
        addItem(items, seen, cratePolicy, {
          path: `${basePath}::${associated.name ?? `#${associatedId}`}`,
          kind: itemKind(associated),
          deprecated: associated.deprecation,
          source: sourceLocation(associated)
        });
      }
    }
  }
}

function collectReexports(json, canonicalPaths, cratePolicy, items, seen, facade) {
  for (const [moduleId, descriptor] of canonicalPaths) {
    if (descriptor.kind !== 'module') continue;
    const module = json.index[moduleId]?.inner?.module;
    if (!module) continue;
    const modulePath = descriptor.path.join('::');
    for (const childId of module.items ?? []) {
      const child = json.index[childId];
      const use = child?.inner?.use;
      if (!use || child.visibility !== 'public') continue;
      const name = use.name ?? child.name;
      if (!name || use.is_glob) continue;
      const targetDescriptor = use.id == null ? null : json.paths[String(use.id)];
      addItem(items, seen, cratePolicy, {
        path: `${modulePath}::${name}`,
        kind: targetDescriptor?.kind ?? 'reexport',
        origin: facade ? 'facade-reexport' : 'reexport',
        target: use.source,
        deprecated: child.deprecation,
        source: sourceLocation(child)
      });
    }
  }
}

function countBy(items, field) {
  return Object.fromEntries(
    [...items.reduce((map, item) => map.set(item[field], (map.get(item[field]) ?? 0) + 1), new Map())]
      .sort(([left], [right]) => left.localeCompare(right))
  );
}

function buildInventory() {
  const policy = readJson(policyPath);
  const metadata = JSON.parse(run('cargo', ['metadata', '--no-deps', '--format-version', '1', '--locked']));
  const products = metadata.packages
    .map((pkg) => ({
      pkg,
      target: pkg.targets.find((target) => target.kind.includes('lib') || target.kind.includes('proc-macro'))
    }))
    .filter(({ pkg, target }) => {
      const publishEnabled = pkg.publish === null || (Array.isArray(pkg.publish) && pkg.publish.length > 0);
      return pkg.name.startsWith('ranvier') && publishEnabled && target;
    })
    .sort((left, right) => left.pkg.name.localeCompare(right.pkg.name));

  const policyCrates = Object.keys(policy.crates).sort();
  const productNames = products.map(({ pkg }) => pkg.name);
  const missingPolicies = productNames.filter((name) => !policy.crates[name]);
  const stalePolicies = policyCrates.filter((name) => !productNames.includes(name));
  if (missingPolicies.length || stalePolicies.length) {
    throw new Error(`API policy/product set drift; missing=${missingPolicies.join(',') || 'none'} stale=${stalePolicies.join(',') || 'none'}`);
  }
  if (policy.product_version !== metadata.packages.find((pkg) => pkg.name === 'ranvier-core')?.version) {
    throw new Error('API policy product_version does not match ranvier-core');
  }

  const crates = [];
  const rustdocFormats = new Set();
  for (const { pkg, target } of products) {
    console.log(`[rustdoc] ${pkg.name}`);
    run(
      'cargo',
      ['rustdoc', '-p', pkg.name, '--lib', '--locked', '--all-features', '--', '-Z', 'unstable-options', '--output-format', 'json'],
      { env: { ...process.env, RUSTC_BOOTSTRAP: '1' }, inherit: true }
    );
    const rustdocPath = path.join(metadata.target_directory, 'doc', `${target.name}.json`);
    if (!existsSync(rustdocPath)) throw new Error(`rustdoc JSON missing for ${pkg.name}: ${rustdocPath}`);
    const json = readJson(rustdocPath);
    rustdocFormats.add(json.format_version);
    const canonicalPaths = Object.entries(json.paths)
      .filter(([, descriptor]) => descriptor.crate_id === 0)
      .sort(([, left], [, right]) => left.path.join('::').localeCompare(right.path.join('::')));
    const items = [];
    const seen = new Set();
    const cratePolicy = { ...policy.crates[pkg.name], deprecations: policy.deprecations };

    for (const [id, descriptor] of canonicalPaths) {
      const item = json.index[id];
      addItem(items, seen, cratePolicy, {
        path: descriptor.path.join('::'),
        kind: descriptor.kind,
        deprecated: item?.deprecation,
        source: sourceLocation(item)
      });
    }
    collectAssociatedItems(json, canonicalPaths, cratePolicy, items, seen);
    collectReexports(json, canonicalPaths, cratePolicy, items, seen, pkg.name === 'ranvier');
    items.sort((left, right) => left.path.localeCompare(right.path) || left.kind.localeCompare(right.kind));

    const unowned = items.filter((item) => !item.owner);
    const invalidTiers = items.filter((item) => !policy.tiers.includes(item.stability));
    if (unowned.length || invalidTiers.length) {
      throw new Error(`${pkg.name} has unclassified items: owner=${unowned.length} tier=${invalidTiers.length}`);
    }

    crates.push({
      package: pkg.name,
      crate_root: target.name,
      version: pkg.version,
      manifest: relative(pkg.manifest_path),
      default_stability: cratePolicy.default_tier,
      owner: cratePolicy.owner,
      features: Object.keys(pkg.features).sort(),
      public_item_count: items.length,
      counts_by_kind: countBy(items, 'kind'),
      counts_by_stability: countBy(items, 'stability'),
      facade_reexport_count: items.filter((item) => item.origin === 'facade-reexport').length,
      boundary_item_count: items.filter((item) => item.boundary).length,
      items
    });
  }

  const inputFiles = [path.join(workspaceRoot, 'Cargo.toml'), policyPath, ...products.map(({ pkg }) => pkg.manifest_path)];
  const inputIdentity = Object.fromEntries(
    inputFiles
      .map((file) => [relative(file), sha256(canonicalText(readFileSync(file)))])
      .sort(([left], [right]) => left.localeCompare(right))
  );
  const allItems = crates.flatMap((entry) => entry.items);
  return {
    schema_version: '1.0.0',
    _generated: {
      notice: 'Generated by scripts/api_surface_inventory.mjs; do not edit by hand.',
      product_set_rule: 'cargo metadata library/proc-macro packages named ranvier* with publish enabled',
      rustdoc_mode: 'all features; JSON generated with stable rustdoc plus RUSTC_BOOTSTRAP=1 until JSON output is stable',
      policy: '.ranvier-api-policy.json'
    },
    product_version: policy.product_version,
    rustc: run('rustc', ['--version']).trim(),
    rustdoc_format_versions: [...rustdocFormats].sort((left, right) => left - right),
    input_sha256: inputIdentity,
    product_crate_count: crates.length,
    public_item_count: allItems.length,
    counts_by_stability: countBy(allItems, 'stability'),
    crates
  };
}

try {
  const inventory = buildInventory();
  const serialized = `${JSON.stringify(inventory, null, 2)}\n`;
  if (mode === '--write') {
    writeFileSync(outputPath, serialized, 'utf8');
    console.log(`wrote ${relative(outputPath)} (${inventory.product_crate_count} crates, ${inventory.public_item_count} public items)`);
  } else {
    if (!existsSync(outputPath)) throw new Error('api-surface-inventory.json is missing; run with --write');
    if (canonicalText(readFileSync(outputPath)) !== serialized) {
      throw new Error('api-surface-inventory.json has drifted; run with --write and review the diff');
    }
    console.log(`API surface inventory: current (${inventory.product_crate_count} crates, ${inventory.public_item_count} public items)`);
  }
} catch (error) {
  console.error(`API surface inventory: FAILED\n${error.message}`);
  process.exitCode = 1;
}
