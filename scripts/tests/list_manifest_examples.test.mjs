import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const scriptPath = fileURLToPath(
  new URL("../list_manifest_examples.mjs", import.meta.url)
);

async function createWorkspaceFixture(t) {
  const root = await mkdtemp(path.join(os.tmpdir(), "ranvier-example-manifest-"));
  t.after(() => rm(root, { recursive: true, force: true }));

  await writeFile(
    path.join(root, "Cargo.toml"),
    [
      "[workspace]",
      'members = ["examples/declared", "examples/unlisted"]',
      'resolver = "2"',
      "",
    ].join("\n")
  );

  for (const name of ["declared", "unlisted"]) {
    const packageRoot = path.join(root, "examples", name);
    await mkdir(path.join(packageRoot, "src"), { recursive: true });
    await writeFile(
      path.join(packageRoot, "Cargo.toml"),
      [
        "[package]",
        `name = "${name}"`,
        'version = "0.1.0"',
        'edition = "2021"',
        "",
      ].join("\n")
    );
    await writeFile(path.join(packageRoot, "src", "main.rs"), "fn main() {}\n");
  }

  return root;
}

function runVerifier(root, examples) {
  const manifestPath = path.join(root, "examples-manifest.json");
  const manifest = { examples };
  return writeFile(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`).then(
    () =>
      spawnSync(
        process.execPath,
        [
          scriptPath,
          "--manifest",
          "examples-manifest.json",
          "--verify-workspace-members",
          "--format",
          "json",
        ],
        { cwd: root, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] }
      )
  );
}

test("workspace verification requires exact manifest and example-member parity", async (t) => {
  const root = await createWorkspaceFixture(t);
  const result = await runVerifier(root, [
    { id: "declared", package: "declared" },
    { id: "unlisted", package: "unlisted" },
  ]);

  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(JSON.parse(result.stdout), ["declared", "unlisted"]);
});

test("workspace verification rejects an unlisted Cargo example member", async (t) => {
  const root = await createWorkspaceFixture(t);
  const result = await runVerifier(root, [
    { id: "declared", package: "declared" },
  ]);

  assert.equal(result.status, 1);
  assert.match(result.stderr, /unlisted: Cargo example workspace member is missing/);
});

test("workspace verification rejects a manifest path mapped to another package", async (t) => {
  const root = await createWorkspaceFixture(t);
  const result = await runVerifier(root, [
    { id: "declared", package: "declared", path: "examples/unlisted" },
    { id: "unlisted", package: "unlisted" },
  ]);

  assert.equal(result.status, 1);
  assert.match(result.stderr, /declared: manifest Cargo path/);
  assert.match(result.stderr, /does not match workspace member path/);
});
