import test from "node:test";
import { createServer } from "node:http";
import assert from "node:assert/strict";
import { mkdtemp, mkdir, writeFile, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { createHash } from "node:crypto";
const exec = promisify(execFile);
const script = resolve(dirname(fileURLToPath(import.meta.url)), "publish.mjs");
const { version } = JSON.parse(
  await readFile(new URL("../vhs-rs/package.json", import.meta.url)),
);
const names = [
  "@cbxss/vhs-rs-linux-x64",
  "@cbxss/vhs-rs-darwin-arm64",
  "@cbxss/vhs-rs",
];

test("release ordering, identical retries, mismatch rejection and missing artifacts", async (t) => {
  const dir = await mkdtemp(join(tmpdir(), "vhs-release-test-"));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const artifacts = join(dir, "artifacts/build");
  const source = join(dir, "source/package");
  const bin = join(dir, "bin");
  await mkdir(artifacts, { recursive: true });
  await mkdir(source, { recursive: true });
  await mkdir(bin);
  const meta = {};
  for (const name of names) {
    const file = join(artifacts, `${name.split("/")[1]}.tgz`);
    await writeFile(
      join(source, "package.json"),
      JSON.stringify({ name, version }),
    );
    await exec("tar", ["-czf", file, "-C", join(dir, "source"), "package"]);
    meta[file] = {
      name,
      integrity: `sha512-${createHash("sha512")
        .update(await readFile(file))
        .digest("base64")}`,
    };
  }
  await writeFile(join(dir, "meta.json"), JSON.stringify(meta));
  await writeFile(join(dir, "remote.json"), "{}");
  await writeFile(
    join(bin, "npm"),
    `#!${process.execPath}
const fs = require('node:fs');
const dir=process.env.VHS_RELEASE_FIXTURE;
const meta=JSON.parse(fs.readFileSync(dir+'/meta.json'));
const remote=JSON.parse(fs.readFileSync(dir+'/remote.json'));
const args=process.argv.slice(2);
if(args[0]==='view') { const name=args[1].slice(0,args[1].lastIndexOf('@')); if(!remote[name]) {console.error('E404');process.exit(1);} console.log(JSON.stringify(remote[name])); }
else if(args[0]==='publish') { const item=meta[args[1]]; if(!item) throw new Error('Unexpected artifact');remote[item.name]=item.integrity;fs.writeFileSync(dir+'/remote.json',JSON.stringify(remote));fs.appendFileSync(dir+'/order',item.name+'\\n'); }
else throw new Error('Unexpected npm command');
`,
    { mode: 0o755 },
  );
  // Aggregate package documents deliberately stay unavailable. Exact-version
  // reads become visible after the fake npm publisher records the upload.
  const server = createServer(async (request, response) => {
    const path = decodeURIComponent(
      new URL(request.url, "http://localhost").pathname,
    );
    const remote = JSON.parse(await readFile(join(dir, "remote.json")));
    const name = path.slice(1, -`/${version}`.length);
    response.setHeader("content-type", "application/json");
    if (!path.endsWith(`/${version}`) || !remote[name]) {
      response.writeHead(404);
      response.end('{"error":"Not found"}');
    } else {
      response.end(
        JSON.stringify({
          name,
          version,
          dist: { integrity: remote[name] },
        }),
      );
    }
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  t.after(async () => {
    server.closeAllConnections();
    await new Promise((resolve) => server.close(resolve));
  });
  const env = {
    ...process.env,
    PATH: `${bin}:${process.env.PATH}`,
    VHS_RELEASE_FIXTURE: dir,
    NPM_CONFIG_REGISTRY: `http://127.0.0.1:${server.address().port}/`,
    GITHUB_REF_NAME: `v${version}`,
  };
  const args = [script, join(dir, "artifacts"), "--publish"];
  await exec(process.execPath, args, { env });
  assert.deepEqual(
    (await readFile(join(dir, "order"), "utf8")).trim().split("\n"),
    names,
  );
  await exec(process.execPath, args, { env });
  assert.equal(
    (await readFile(join(dir, "order"), "utf8")).trim().split("\n").length,
    3,
  );
  const remote = JSON.parse(await readFile(join(dir, "remote.json")));
  remote[names[0]] = "sha512-wrong";
  await writeFile(join(dir, "remote.json"), JSON.stringify(remote));
  await assert.rejects(exec(process.execPath, args, { env }), (error) =>
    error.stderr.includes("different contents"),
  );
  await rm(join(artifacts, "vhs-rs-darwin-arm64.tgz"));
  await assert.rejects(exec(process.execPath, args, { env }), (error) =>
    error.stderr.includes("both platform packages"),
  );
  await assert.rejects(
    exec(process.execPath, args, {
      env: { ...env, GITHUB_REF_NAME: "v9.9.9" },
    }),
    (error) => error.stderr.includes("Git tag"),
  );
});
