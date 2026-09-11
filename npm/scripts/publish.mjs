// With no --publish this only validates and prints the exact release contents.
import { readFile, readdir } from "node:fs/promises";
import { createHash } from "node:crypto";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { join, resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
const exec = promisify(execFile);
const root = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const version = JSON.parse(
  await readFile(join(root, "npm/vhs-rs/package.json")),
).version;
const releaseDir = resolve(process.argv[2] ?? "release-artifacts");
const publish = process.argv.includes("--publish");
const expectedTag = version.includes("-") ? "next" : "latest";
if (
  process.env.GITHUB_REF_NAME &&
  process.env.GITHUB_REF_NAME !== `v${version}`
)
  throw new Error("Git tag and package version differ");
const packages = new Map();
for (const dir of await readdir(releaseDir)) {
  for (const file of await readdir(join(releaseDir, dir))) {
    if (!file.endsWith(".tgz")) continue;
    const path = join(releaseDir, dir, file);
    const manifest = JSON.parse(
      (await exec("tar", ["-xOf", path, "package/package.json"])).stdout,
    );
    const integrity = `sha512-${createHash("sha512")
      .update(await readFile(path))
      .digest("base64")}`;
    if (manifest.version !== version)
      throw new Error(`Unexpected version: ${file}`);
    const existing = packages.get(manifest.name);
    if (existing && existing.integrity !== integrity)
      throw new Error(`Different builds of ${manifest.name}`);
    packages.set(manifest.name, { path, integrity });
  }
}
const order = [
  "@cbxss/vhs-rs-linux-x64",
  "@cbxss/vhs-rs-darwin-arm64",
  "@cbxss/vhs-rs",
];
if (packages.size !== order.length || order.some((name) => !packages.has(name)))
  throw new Error("Expected the SDK and both platform packages");
// Read the exact version: the aggregate package document used by npm view can
// lag a successful publication and return a cached 404 for a brand-new package.
const registry =
  process.env.NPM_CONFIG_REGISTRY ?? "https://registry.npmjs.org/";
async function publishedIntegrity(name) {
  const url = new URL(
    `${name.replace("/", "%2f")}/${version}`,
    registry.endsWith("/") ? registry : registry + "/",
  );
  const response = await fetch(url, {
    headers: { "cache-control": "no-cache" },
    signal: AbortSignal.timeout(10000),
  });
  if (response.status === 404) return undefined;
  if (!response.ok)
    throw new Error(
      `Registry returned ${response.status} for ${name}@${version}`,
    );
  const pkg = await response.json();
  if (
    pkg.name !== name ||
    pkg.version !== version ||
    typeof pkg.dist?.integrity !== "string"
  ) {
    throw new Error(`Invalid registry metadata for ${name}@${version}`);
  }
  return pkg.dist.integrity;
}

for (const name of order) {
  const { path, integrity } = packages.get(name);
  console.log(`${name}@${version}: ${path} (${integrity})`);
  if (!publish) continue;
  const remote = await publishedIntegrity(name);
  if (remote) {
    if (remote !== integrity)
      throw new Error(
        `${name}@${version} already exists with different contents`,
      );
    console.log("Already published with identical contents; skipping");
  } else {
    const result = await exec("npm", [
      "publish",
      path,
      "--access",
      "public",
      "--tag",
      expectedTag,
      "--ignore-scripts",
    ]);
    process.stdout.write(result.stdout);
  }
  // The SDK is not published until both platform versions are visible.
  let visible = false;
  for (let attempt = 0; attempt < 30; attempt++) {
    const result = await publishedIntegrity(name);
    if (result !== undefined) {
      if (result !== integrity)
        throw new Error(`Published integrity differs for ${name}`);
      visible = true;
      break;
    }
    await new Promise((resolve) => setTimeout(resolve, 2000));
  }
  if (!visible)
    throw new Error(`${name}@${version} is not visible yet; rerun the release`);
}
