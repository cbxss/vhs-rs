import test from "node:test";
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { mkdtemp, mkdir, writeFile, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createHash } from "node:crypto";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import {
  validateReceipt,
  publishedChecksum,
  checkPublished,
} from "./crate-release.mjs";

const exec = promisify(execFile);
const bytes = Buffer.from("verified crate bytes");
const sha = "a".repeat(40);
const checksum = createHash("sha256").update(bytes).digest("hex");
const receipt = {
  schema: 1,
  crate: "vhs-rs",
  version: "0.3.2",
  source_sha: sha,
  toolchain: "1.94.1",
  sha256: checksum,
};

test("crate receipt binds bytes, version, source and toolchain", () => {
  validateReceipt(receipt, bytes, "0.3.2", sha);
  for (const change of [
    { schema: 2 },
    { crate: "other" },
    { version: "0.3.1" },
    { source_sha: "b".repeat(40) },
    { toolchain: "../../bad" },
    { sha256: "b".repeat(64) },
  ]) {
    assert.throws(
      () => validateReceipt({ ...receipt, ...change }, bytes, "0.3.2", sha),
      /Invalid crate receipt/,
    );
  }
  assert.throws(
    () => validateReceipt(receipt, Buffer.from("altered"), "0.3.2", sha),
    /Invalid crate receipt/,
  );
  assert.equal(checkPublished(undefined, checksum), false);
  assert.equal(checkPublished(checksum, checksum), true);
  assert.throws(
    () => checkPublished("b".repeat(64), checksum),
    /different contents/,
  );
});

test("registry reads distinguish absence, rejection and invalid metadata", async (t) => {
  let status = 404;
  let metadata;
  const server = createServer((req, res) => {
    assert.equal(req.url, "/api/v1/crates/vhs-rs/0.3.2");
    res.writeHead(status, { "content-type": "application/json" });
    res.end(JSON.stringify(metadata ?? {}));
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  t.after(() => {
    server.closeAllConnections();
    server.close();
  });
  const registry = `http://127.0.0.1:${server.address().port}/`;
  assert.equal(await publishedChecksum(registry, "0.3.2"), undefined);
  status = 403;
  await assert.rejects(publishedChecksum(registry, "0.3.2"), /403/);
  status = 200;
  await assert.rejects(
    publishedChecksum(registry, "0.3.2"),
    /Invalid crates registry metadata/,
  );
  metadata = { version: { crate: "vhs-rs", num: "0.3.2", checksum } };
  assert.equal(await publishedChecksum(registry, "0.3.2"), checksum);
});

test("crate CLI rejects missing artifacts and identity errors, skips identical retries without Cargo", async (t) => {
  const dir = await mkdtemp(join(tmpdir(), "vhs-crate-release-"));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const artifacts = join(dir, "release-linux-x64");
  await mkdir(artifacts);
  const { version } = JSON.parse(
    await readFile(new URL("../vhs-rs/package.json", import.meta.url)),
  );
  const source = (await exec("git", ["rev-parse", "HEAD"])).stdout.trim();
  const meta = { ...receipt, version, source_sha: source };
  const script = new URL("./crate-release.mjs", import.meta.url).pathname;
  const bin = join(dir, "bin");
  await mkdir(bin);
  await writeFile(
    join(bin, "cargo"),
    '#!/bin/sh\necho "UNEXPECTED_CARGO_MUTATION" >&2\nexit 99\n',
    { mode: 0o755 },
  );
  const server = createServer((req, res) => {
    res.writeHead(200, { "content-type": "application/json" });
    res.end(
      JSON.stringify({ version: { crate: "vhs-rs", num: version, checksum } }),
    );
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  t.after(() => {
    server.closeAllConnections();
    server.close();
  });
  const env = {
    ...process.env,
    RELEASE_TAG: `v${version}`,
    VHS_CRATES_REGISTRY: `http://127.0.0.1:${server.address().port}/`,
    PATH: `${bin}:${process.env.PATH}`,
  };
  const args = [script, "publish", dir];
  await assert.rejects(exec(process.execPath, args, { env }), (e) =>
    e.stderr.includes("ENOENT"),
  );
  await writeFile(join(artifacts, "crate-receipt.json"), JSON.stringify(meta));
  await writeFile(join(artifacts, `vhs-rs-${version}.crate`), bytes);
  assert.match(
    (await exec(process.execPath, args, { env })).stdout,
    /identical contents; skipping/,
  );
  await assert.rejects(
    exec(process.execPath, args, { env: { ...env, RELEASE_TAG: "v9.9.9" } }),
    (e) => e.stderr.includes("tag and crate version differ"),
  );
  await writeFile(
    join(artifacts, "crate-receipt.json"),
    JSON.stringify({ ...meta, source_sha: "b".repeat(40) }),
  );
  await assert.rejects(exec(process.execPath, args, { env }), (e) =>
    e.stderr.includes("Invalid crate receipt"),
  );
});
