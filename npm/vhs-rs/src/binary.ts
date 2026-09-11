import { createRequire } from "node:module";
import { accessSync, constants, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { VhsError } from "./errors.js";

const require = createRequire(import.meta.url);
export const version: string = JSON.parse(
  readFileSync(new URL("../package.json", import.meta.url), "utf8"),
).version;

export function resolveBinary(
  platform = process.platform,
  arch = process.arch,
): string {
  const packages: Record<string, string> = {
    "linux-x64": "@cbxss/vhs-rs-linux-x64",
    "darwin-arm64": "@cbxss/vhs-rs-darwin-arm64",
  };
  const name = packages[`${platform}-${arch}`];
  if (!name)
    throw new VhsError(
      `Unsupported platform ${platform}-${arch}; supported: Linux x64 and macOS ARM64`,
      "unsupported_platform",
    );
  let manifest: string;
  try {
    manifest = require.resolve(`${name}/package.json`);
  } catch {
    throw new VhsError(
      `Missing ${name}@${version}. Reinstall with optional dependencies enabled (npm install --include=optional).`,
      "missing_binary",
    );
  }
  const installed = JSON.parse(readFileSync(manifest, "utf8"));
  if (installed.version !== version)
    throw new VhsError(
      `Binary package ${name} is ${installed.version}; expected ${version}`,
      "version_mismatch",
    );
  const binary = join(dirname(manifest), "bin", "vhs-rs");
  try {
    accessSync(binary, constants.X_OK);
  } catch {
    throw new VhsError(
      `Binary is missing or not executable: ${binary}. Reinstall ${name}.`,
      "missing_binary",
    );
  }
  return binary;
}
