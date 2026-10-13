// Cargo repackages before uploading. Bind that package to the original verified
// artifact, source commit, and toolchain; retries never replace different bytes.
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { readFile, writeFile, mkdir, cp, appendFile } from "node:fs/promises";
import { createHash } from "node:crypto";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const exec = promisify(execFile);
const root = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const hash = (bytes) => createHash("sha256").update(bytes).digest("hex");

export function validateReceipt(receipt, bytes, version, sha) {
  if (
    receipt.schema !== 1 ||
    receipt.crate !== "vhs-rs" ||
    receipt.version !== version ||
    receipt.source_sha !== sha ||
    !/^[0-9a-f]{40}$/.test(sha) ||
    !/^\d+\.\d+\.\d+$/.test(receipt.toolchain) ||
    !/^[0-9a-f]{64}$/.test(receipt.sha256) ||
    receipt.sha256 !== hash(bytes)
  )
    throw new Error(
      "Invalid crate receipt: version, source, toolchain, or checksum differs",
    );
}

export async function publishedChecksum(registry, version) {
  const response = await fetch(
    new URL(
      `api/v1/crates/vhs-rs/${version}`,
      registry.endsWith("/") ? registry : `${registry}/`,
    ),
    {
      headers: {
        "user-agent": "vhs-rs-release (https://github.com/cbxss/vhs-rs)",
        "cache-control": "no-cache",
      },
      signal: AbortSignal.timeout(10000),
    },
  );
  if (response.status === 404) return undefined;
  if (!response.ok)
    throw new Error(`Crates registry returned ${response.status}`);
  const { version: pkg } = await response.json();
  if (
    pkg?.crate !== "vhs-rs" ||
    pkg.num !== version ||
    !/^[0-9a-f]{64}$/.test(pkg.checksum)
  )
    throw new Error("Invalid crates registry metadata");
  return pkg.checksum;
}

export function checkPublished(checksum, expected) {
  if (checksum !== undefined && checksum !== expected)
    throw new Error("Crate already exists with different contents");
  return checksum !== undefined;
}

async function main() {
  const [mode, artifactArg] = process.argv.slice(2);
  if (!["stage", "inspect", "publish"].includes(mode) || !artifactArg)
    throw new Error(
      "Usage: crate-release.mjs <stage|inspect|publish> <artifact directory>",
    );
  const version = /^version = "([^"]+)"/m.exec(
    await readFile(join(root, "Cargo.toml"), "utf8"),
  )?.[1];
  if (!/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(version))
    throw new Error("Invalid crate version");
  const sha = (
    await exec("git", ["rev-parse", "HEAD"], { cwd: root })
  ).stdout.trim();
  const filename = `vhs-rs-${version}.crate`;
  const directory = resolve(artifactArg);
  if (mode === "stage") {
    await exec("cargo", ["package", "--locked"], {
      cwd: root,
      maxBuffer: 4 * 1024 * 1024,
    });
    const path = join(root, "target/package", filename);
    const vcs = JSON.parse(
      (
        await exec("tar", [
          "-xOf",
          path,
          `vhs-rs-${version}/.cargo_vcs_info.json`,
        ])
      ).stdout,
    );
    if (vcs.git.sha1 !== sha || vcs.git.dirty)
      throw new Error("Crate must come from a clean source checkout");
    const toolchain = (await exec("rustc", ["--version"])).stdout.split(" ")[1];
    const bytes = await readFile(path);
    const receipt = {
      schema: 1,
      crate: "vhs-rs",
      version,
      source_sha: sha,
      toolchain,
      sha256: hash(bytes),
    };
    validateReceipt(receipt, bytes, version, sha);
    await mkdir(directory, { recursive: true });
    await cp(path, join(directory, filename));
    await writeFile(
      join(directory, "crate-receipt.json"),
      JSON.stringify(receipt, null, 2) + "\n",
    );
    console.log(`Verified ${filename} at ${sha}: ${receipt.sha256}`);
    return;
  }
  const crateDir = join(directory, "release-linux-x64");
  const receipt = JSON.parse(
    await readFile(join(crateDir, "crate-receipt.json")),
  );
  validateReceipt(
    receipt,
    await readFile(join(crateDir, filename)),
    version,
    sha,
  );
  if (process.env.RELEASE_TAG !== `v${version}`)
    throw new Error("Release tag and crate version differ");
  const registry = process.env.VHS_CRATES_REGISTRY ?? "https://crates.io/";
  const remote = await publishedChecksum(registry, version);
  const exists = checkPublished(remote, receipt.sha256);
  if (mode === "inspect") {
    if (process.env.GITHUB_OUTPUT)
      await appendFile(
        process.env.GITHUB_OUTPUT,
        `upload=${!exists}\ntoolchain=${receipt.toolchain}\n`,
      );
    console.log(
      exists
        ? "Crate already published with identical contents"
        : `Crate upload needed using Rust ${receipt.toolchain}`,
    );
    return;
  }
  if (exists) {
    console.log("Crate already published with identical contents; skipping");
    return;
  }
  await exec("cargo", [`+${receipt.toolchain}`, "package", "--locked"], {
    cwd: root,
    maxBuffer: 4 * 1024 * 1024,
  });
  if (
    hash(await readFile(join(root, "target/package", filename))) !==
    receipt.sha256
  )
    throw new Error("Repackaged crate differs from the verified artifact");
  const result = await exec(
    "cargo",
    [`+${receipt.toolchain}`, "publish", "--locked"],
    { cwd: root, maxBuffer: 4 * 1024 * 1024 },
  );
  process.stdout.write(result.stdout);
  process.stderr.write(result.stderr);
  for (let attempt = 0; attempt < 30; attempt++) {
    const visible = await publishedChecksum(registry, version);
    if (visible !== undefined) {
      checkPublished(visible, receipt.sha256);
      console.log(`Published crate checksum verified: ${receipt.sha256}`);
      return;
    }
    await new Promise((resolve) => setTimeout(resolve, 2000));
  }
  throw new Error(
    "Crate is not visible yet; retry with the original artifacts",
  );
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href)
  await main();
