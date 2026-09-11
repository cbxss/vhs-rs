// Serve only the packed release artifacts. This exercises npm's actual optional
// dependency selection without publishing test packages or using workspace links.
import { createServer } from "node:http";
import { createHash } from "node:crypto";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import {
  mkdtemp,
  readFile,
  writeFile,
  readdir,
  rm,
  mkdir,
  access,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
const exec = promisify(execFile);
const root = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const consumer = await mkdtemp(join(tmpdir(), "vhs-npm-consumer-"));
const artifacts = join(root, "npm/artifacts");
const packages = new Map();
const tarballs = new Map();
for (const file of await readdir(artifacts)) {
  if (!file.endsWith(".tgz")) continue;
  const bytes = await readFile(join(artifacts, file));
  const { stdout } = await exec("tar", [
    "-xOf",
    join(artifacts, file),
    "package/package.json",
  ]);
  const pkg = JSON.parse(stdout);
  tarballs.set(`/${file}`, bytes);
  packages.set(pkg.name, {
    pkg,
    file,
    integrity: `sha512-${createHash("sha512").update(bytes).digest("base64")}`,
  });
}
let registry;
const server = createServer((req, res) => {
  const path = decodeURIComponent(req.url.split("?")[0]);
  const bytes = tarballs.get(path);
  if (bytes) {
    res.end(bytes);
    return;
  }
  const entry = packages.get(path.slice(1));
  if (!entry) {
    res.writeHead(404);
    res.end("{}");
    return;
  }
  const { pkg, file, integrity } = entry;
  res.setHeader("content-type", "application/json");
  res.end(
    JSON.stringify({
      name: pkg.name,
      "dist-tags": { latest: pkg.version },
      versions: {
        [pkg.version]: {
          ...pkg,
          dist: { tarball: `${registry}/${file}`, integrity },
        },
      },
    }),
  );
});
try {
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  registry = `http://127.0.0.1:${server.address().port}`;
  await writeFile(
    join(consumer, "package.json"),
    JSON.stringify({ name: "vhs-consumer", private: true, type: "module" }),
  );
  const guard = join(consumer, "guard");
  await mkdir(guard);
  for (const name of ["cargo", "rustc", "vhs-rs"]) {
    await writeFile(
      join(guard, name),
      '#!/bin/sh\necho invoked >> "$VHS_FORBIDDEN_MARKER"\nexit 99\n',
      { mode: 0o755 },
    );
  }
  const marker = join(consumer, "forbidden-invocation");
  const env = {
    ...process.env,
    PATH: `${guard}:${process.env.PATH}`,
    VHS_FORBIDDEN_MARKER: marker,
    VHS_TEST_PACKAGE_ROOT: consumer,
  };
  const version = packages.get("@cbxss/vhs-rs").pkg.version;
  const installed = await exec(
    "npm",
    [
      "install",
      `@cbxss/vhs-rs@${version}`,
      "--ignore-scripts",
      "--include=optional",
      "--no-audit",
      "--no-fund",
      "--registry",
      registry,
      "--cache",
      join(consumer, "cache"),
    ],
    { cwd: consumer, env },
  );
  console.log(installed.stdout.trim());
  server.closeAllConnections();
  await new Promise((resolve) => server.close(resolve));
  // The registry is offline for all consumer execution below.
  await writeFile(
    join(consumer, "consumer.mts"),
    `import { check, createSession, run, type RunReport } from '@cbxss/vhs-rs';
const result: RunReport = await run({ tape: '' });
if (result.status !== 'success') throw new Error('run failed');
const session = await createSession({ typingSpeedMs: 0 });
try { await session.type('echo typed'); await session.press('Enter'); await session.waitFor(); }
finally { await session.close(); }
console.log(await check({ tape: 'Screen' }));
`,
  );
  await exec(
    process.execPath,
    [
      join(root, "npm/node_modules/typescript/bin/tsc"),
      "--strict",
      "--target",
      "ES2022",
      "--module",
      "NodeNext",
      "--moduleResolution",
      "NodeNext",
      "consumer.mts",
    ],
    { cwd: consumer, env },
  );
  await exec(process.execPath, ["consumer.mjs"], {
    cwd: consumer,
    env,
    timeout: 30000,
  });
  const cli = await exec(
    join(consumer, "node_modules/.bin/vhs-rs"),
    ["--version"],
    { cwd: consumer, env },
  );
  if (cli.stdout.trim() !== `vhs-rs ${version}`)
    throw new Error("CLI version mismatch");
  const tests = (await readdir(join(root, "npm/vhs-rs/test")))
    .filter((f) => f.endsWith(".test.mjs"))
    .map((f) => join(root, "npm/vhs-rs/test", f));
  try {
    const output = await exec(process.execPath, ["--test", ...tests], {
      cwd: consumer,
      env,
      timeout: 180000,
      maxBuffer: 4 * 1024 * 1024,
    });
    console.log(output.stdout);
  } catch (error) {
    console.error(error.stdout, error.stderr);
    throw error;
  }
  let forbidden = false;
  try {
    await access(marker);
    forbidden = true;
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
  }
  if (forbidden)
    throw new Error("Consumer invoked Cargo, rustc, or a global vhs-rs");
  console.log(
    "Packed package verified offline; no Rust compiler or global binary invoked.",
  );
} finally {
  server.closeAllConnections();
  server.close();
  await rm(consumer, { recursive: true, force: true });
}
