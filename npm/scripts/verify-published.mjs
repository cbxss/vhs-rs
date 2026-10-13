// Fresh registry and installer downloads, separate from pre-release fixture
// tests. Installer and npm binaries must match the original verified artifact.
import { mkdtemp, mkdir, readFile, readdir, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { createHash } from "node:crypto";
import assert from "node:assert/strict";

const exec = promisify(execFile);
const root = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const version = JSON.parse(
  await readFile(join(root, "npm/vhs-rs/package.json")),
).version;
const platform =
  process.env.RELEASE_PLATFORM ??
  `${process.platform === "darwin" ? "darwin" : "linux"}-${process.arch}`;
const target = {
  "linux-x64": "x86_64-unknown-linux-musl",
  "darwin-arm64": "aarch64-apple-darwin",
}[platform];
if (!target || process.env.RELEASE_TAG !== `v${version}`)
  throw new Error("Invalid published release platform or tag");
const artifacts = resolve(process.argv[2] ?? "release-artifacts");
const scratch = await mkdtemp(join(tmpdir(), "vhs-published-"));
const hash = (bytes) => createHash("sha256").update(bytes).digest("hex");
const run = async (command, args, options = {}) => {
  const result = await exec(command, args, {
    cwd: root,
    timeout: 180000,
    maxBuffer: 4 * 1024 * 1024,
    ...options,
  });
  process.stdout.write(result.stdout);
  return result;
};

try {
  const archive = join(
    artifacts,
    `release-${platform}`,
    `vhs-rs-${target}.tar.gz`,
  );
  const original = (
    await exec("tar", ["-xOf", archive, `vhs-rs-${target}/vhs-rs`], {
      encoding: "buffer",
      maxBuffer: 32 * 1024 * 1024,
    })
  ).stdout;
  const install = join(scratch, "installer");
  await run("sh", [join(root, "install.sh")], {
    env: {
      ...process.env,
      VHS_RS_VERSION: `v${version}`,
      VHS_RS_INSTALL_DIR: install,
    },
  });
  const installedBinary = join(install, "vhs-rs");
  assert.equal(
    hash(await readFile(installedBinary)),
    hash(original),
    "Installer binary differs from verified artifact",
  );
  await run(process.execPath, [
    join(root, "npm/scripts/test-examples.mjs"),
    installedBinary,
  ]);

  const consumer = join(scratch, "consumer");
  await mkdir(consumer);
  await run(
    "npm",
    [
      "install",
      `@cbxss/vhs-rs@${version}`,
      "--ignore-scripts",
      "--include=optional",
      "--no-audit",
      "--no-fund",
      "--cache",
      join(scratch, "npm-cache"),
    ],
    { cwd: consumer },
  );
  const binaryModule = await import(
    pathToFileURL(join(consumer, "node_modules/@cbxss/vhs-rs/dist/binary.js"))
  );
  assert.equal(
    hash(await readFile(binaryModule.resolveBinary())),
    hash(original),
    "npm binary differs from verified artifact",
  );
  const tests = (await readdir(join(root, "npm/vhs-rs/test")))
    .filter((file) => file.endsWith(".test.mjs"))
    .map((file) => join(root, "npm/vhs-rs/test", file));
  await run(process.execPath, ["--test", ...tests], {
    cwd: consumer,
    env: { ...process.env, VHS_TEST_PACKAGE_ROOT: consumer },
  });

  const cargo = join(scratch, "cargo");
  await run(
    "cargo",
    ["install", "--locked", "--version", version, "--root", cargo, "vhs-rs"],
    { timeout: 600000 },
  );
  const cargoBinary = join(cargo, "bin/vhs-rs");
  assert.equal(
    (await exec(cargoBinary, ["--version"])).stdout.trim(),
    `vhs-rs ${version}`,
  );
  await run(process.execPath, [
    join(root, "npm/scripts/test-examples.mjs"),
    cargoBinary,
  ]);
  console.log(
    `Published ${version} verified on ${platform}: installer and npm artifact hashes, full SDK tests, and Cargo-built examples.`,
  );
} finally {
  await rm(scratch, { recursive: true, force: true });
}
