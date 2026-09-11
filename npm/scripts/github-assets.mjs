import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { mkdtemp, readdir, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, basename, resolve } from "node:path";
import { createHash } from "node:crypto";
const exec = promisify(execFile);
const [tag, directory] = process.argv.slice(2);
if (!tag || !directory)
  throw new Error("Usage: github-assets.mjs <tag> <artifact directory>");
const { assets } = JSON.parse(
  (await exec("gh", ["release", "view", tag, "--json", "assets"])).stdout,
);
const scratch = await mkdtemp(join(tmpdir(), "vhs-github-assets-"));
const hash = (bytes) => createHash("sha256").update(bytes).digest("hex");
try {
  for (const dir of await readdir(directory)) {
    for (const file of await readdir(join(directory, dir))) {
      if (!file.endsWith(".tar.gz") && !file.endsWith(".sha256")) continue;
      const path = resolve(directory, dir, file);
      if (assets.some((asset) => asset.name === basename(path))) {
        await exec("gh", [
          "release",
          "download",
          tag,
          "--pattern",
          file,
          "--dir",
          scratch,
        ]);
        if (
          hash(await readFile(path)) !==
          hash(await readFile(join(scratch, file)))
        )
          throw new Error(`Existing release asset differs: ${file}`);
      } else {
        await exec("gh", ["release", "upload", tag, path]);
      }
    }
  }
} finally {
  await rm(scratch, { recursive: true, force: true });
}
