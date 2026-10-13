// Executes the public examples against the supplied native binary. CI verifies
// in scratch directories; only the explicit --write-assets mode updates images.
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import {
  mkdtemp,
  mkdir,
  readFile,
  writeFile,
  stat,
  cp,
  rm,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";

const exec = promisify(execFile);
const root = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const binary = resolve(
  process.argv.slice(2).find((arg) => !arg.startsWith("--")) ??
    join(root, "target/debug/vhs-rs"),
);
const writeAssets = process.argv.includes("--write-assets");
const scratch = await mkdtemp(join(tmpdir(), "vhs-examples-"));

async function execute(tape, cwd, expectedExit = 0, extra = []) {
  const args = ["run", "--json", "--timeout", "30s", ...extra, tape];
  let output;
  try {
    output = await exec(binary, args, {
      cwd,
      timeout: 35000,
      maxBuffer: 4 * 1024 * 1024,
    });
    assert.equal(expectedExit, 0, "expected the tape to fail");
  } catch (error) {
    if (error.code !== expectedExit) throw error;
    output = error;
  }
  const report = JSON.parse(output.stdout);
  assert.equal(report.exit_code, expectedExit);
  return report;
}

try {
  const quickstart = join(scratch, "quickstart");
  const showcase = join(scratch, "showcase");
  const failure = join(scratch, "failure");
  for (const dir of [quickstart, showcase, failure]) await mkdir(dir);

  const quick = await execute(
    join(root, "examples/quickstart.tape"),
    quickstart,
  );
  assert.equal(quick.status, "success");
  assert.match(await readFile(join(quickstart, "proof.txt"), "utf8"), /^42$/m);
  for (const file of ["demo.gif", "proof.png"])
    assert.ok((await stat(join(quickstart, file))).size > 100);

  const hero = await execute(join(root, "examples/readme.tape"), showcase, 0, [
    "--record",
    "session.jsonl",
  ]);
  assert.equal(hero.status, "success");
  assert.equal(
    await readFile(join(showcase, "notes.txt"), "utf8"),
    "Script your terminal.\nTest what it shows.\nRecord and replay.\n",
  );
  assert.match(
    await readFile(join(showcase, "proof.txt"), "utf8"),
    /^Saved lines: *3$/m,
  );
  assert.ok((await stat(join(showcase, "readme.gif"))).size < 2 * 1024 * 1024);
  await exec(
    binary,
    ["render", "session.jsonl", "-o", "replay.gif", "-o", "final.txt"],
    { cwd: showcase, timeout: 30000 },
  );
  assert.equal(
    await readFile(join(showcase, "final.txt"), "utf8"),
    await readFile(join(showcase, "proof.txt"), "utf8"),
  );

  const failed = await execute(
    join(root, "examples/failure-demo.tape"),
    failure,
    1,
  );
  assert.equal(failed.status, "assert_failed");
  assert.match(failed.failure.message, /42/);
  assert.match(
    await readFile(join(failure, "failure-demo.failure.txt"), "utf8"),
    /^the answer is 41$/m,
  );
  assert.ok((await stat(join(failure, "failure-demo.failure.png"))).size > 100);

  const negative = join(scratch, "echoed-input.tape");
  await writeFile(
    negative,
    'Set TypingSpeed 0ms\nType "false # 42"\nEnter\nWait\nAssert /(?m)^42$/\n',
  );
  assert.equal((await execute(negative, scratch, 1)).status, "assert_failed");
  await writeFile(
    negative,
    "Set TypingSpeed 0ms\nType \"false; printf 'command-status:%s\\n' $?\"\nEnter\nWait\nAssert /(?m)^command-status:0$/\n",
  );
  assert.equal((await execute(negative, scratch, 1)).status, "assert_failed");
  console.log(
    "Quickstart, editor save, screenshots, replay, intentional failure, and echoed-input guards passed.",
  );

  if (writeAssets) {
    await cp(join(showcase, "readme.gif"), join(root, "examples/readme.gif"));
    await cp(
      join(failure, "failure-demo.failure.png"),
      join(root, "examples/failure-forensics.png"),
    );
    console.log(
      "Updated README GIF and failure screenshot from verified examples.",
    );
  }
} finally {
  await rm(scratch, { recursive: true, force: true });
}
