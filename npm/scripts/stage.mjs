import { readFile, mkdir, cp, chmod, rm } from "node:fs/promises";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { dirname, resolve, join } from "node:path";
import { fileURLToPath } from "node:url";

const exec = promisify(execFile);
const root = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const [target, binaryArg] = process.argv.slice(2);
if (!["linux-x64", "darwin-arm64"].includes(target) || !binaryArg)
  throw new Error("Usage: stage.mjs <linux-x64|darwin-arm64> <binary>");
const binary = resolve(binaryArg);
const manifest = JSON.parse(
  await readFile(join(root, "npm/vhs-rs/package.json")),
);
const cargo = await readFile(join(root, "Cargo.toml"), "utf8");
const version = /^version = "([^"]+)"/m.exec(cargo)?.[1];
if (manifest.version !== version) throw new Error("Cargo/npm versions differ");
const actual = (await exec(binary, ["--version"])).stdout.trim();
if (actual !== `vhs-rs ${version}`) throw new Error(`Wrong binary: ${actual}`);
const artifacts = join(root, "npm/artifacts");
const staging = join(root, "npm/staging", target);
await rm(staging, { recursive: true, force: true });
await mkdir(artifacts, { recursive: true });
for (const name of [`vhs-rs-${target}`, "vhs-rs"]) {
  const dir = join(staging, name);
  await mkdir(dir, { recursive: true });
  const source = join(root, "npm", name);
  await cp(join(source, "package.json"), join(dir, "package.json"));
  const pkg = JSON.parse(await readFile(join(dir, "package.json")));
  if (pkg.version !== version) throw new Error(`${name}: wrong version`);
  if (name === "vhs-rs") {
    if (Object.values(pkg.optionalDependencies).some((v) => v !== version))
      throw new Error("Binary versions must be exact and match SDK");
    await cp(join(source, "dist"), join(dir, "dist"), { recursive: true });
    await chmod(join(dir, "dist/cli.js"), 0o755);
  } else {
    await mkdir(join(dir, "bin"));
    await cp(binary, join(dir, "bin/vhs-rs"));
    await chmod(join(dir, "bin/vhs-rs"), 0o755);
  }
  await cp(join(root, "npm/vhs-rs/README.md"), join(dir, "README.md"));
  await cp(join(root, "LICENSE"), join(dir, "LICENSE"));
  for (const license of ["OFL.txt", "SYMBOLS-LICENSE.txt"])
    await cp(join(root, "assets/fonts", license), join(dir, license));
  const packed = await exec(
    "npm",
    ["pack", "--ignore-scripts", "--json", "--pack-destination", artifacts],
    { cwd: dir },
  );
  console.log(JSON.parse(packed.stdout)[0].filename);
}
// The GitHub installer and npm package ship the same tested executable.
const rustTarget = {
  "linux-x64": "x86_64-unknown-linux-musl",
  "darwin-arm64": "aarch64-apple-darwin",
}[target];
const legacyName = `vhs-rs-${rustTarget}`;
const legacy = join(staging, legacyName);
await mkdir(legacy);
await cp(binary, join(legacy, "vhs-rs"));
await chmod(join(legacy, "vhs-rs"), 0o755);
for (const file of ["LICENSE", "README.md"])
  await cp(join(root, file), join(legacy, file));
for (const file of ["OFL.txt", "SYMBOLS-LICENSE.txt"])
  await cp(join(root, "assets/fonts", file), join(legacy, file));
await exec("tar", [
  "-czf",
  join(artifacts, `${legacyName}.tar.gz`),
  "-C",
  staging,
  legacyName,
]);
const { createHash } = await import("node:crypto");
const { writeFile } = await import("node:fs/promises");
const hash = createHash("sha256")
  .update(await readFile(join(artifacts, `${legacyName}.tar.gz`)))
  .digest("hex");
await writeFile(
  join(artifacts, `${legacyName}.tar.gz.sha256`),
  `${hash}  ${legacyName}.tar.gz\n`,
);
