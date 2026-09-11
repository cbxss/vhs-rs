import test from "node:test";
import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdtemp, writeFile, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { promisify } from "node:util";

const exec = promisify(execFile);
const dist = join(
  process.env.VHS_TEST_PACKAGE_ROOT,
  "node_modules/@cbxss/vhs-rs/dist",
);
const { resolveBinary } = await import(pathToFileURL(join(dist, "binary.js")));
const sdkUrl = pathToFileURL(join(dist, "index.js")).href;

async function fixture(t) {
  const cwd = await mkdtemp(join(tmpdir(), "vhs-shutdown-"));
  t.after(() => rm(cwd, { recursive: true, force: true }));
  const shell = join(cwd, "shell");
  const marker = join(cwd, "cleaned-up");
  // Reproduce the unread-output dependency on any shell/CI image, rather
  // than relying on a particular Homebrew Bash version. The marker proves
  // the exit trap finished; force-killing the shell must not count as a pass.
  await writeFile(
    shell,
    `#!/bin/sh
trap '' TERM
trap 'dd if=/dev/zero bs=65536 count=1 2>/dev/null; printf done > "$VHS_SHUTDOWN_MARKER"; exit 0' HUP
printf 'READY\\n> '
while :; do :; done
`,
    { mode: 0o755 },
  );
  return {
    cwd,
    shellPath: shell,
    marker,
    env: { ...process.env, VHS_SHUTDOWN_MARKER: marker },
    // External watchdog: Rust's run timeout cannot interrupt a blocking
    // waitpid. Kill the subprocess so this regression fails instead of
    // leaving CI waiting forever. This is not a performance assertion.
    timeout: 8000,
    killSignal: "SIGKILL",
  };
}

test("CLI subprocess drains exit output and returns its report", async (t) => {
  const options = await fixture(t);
  const tape = join(options.cwd, "shutdown.tape");
  await writeFile(tape, `Set Shell "${options.shellPath}"\nScreen\n`);
  const { stdout } = await exec(
    resolveBinary(),
    ["run", "--json", "--timeout", "5s", tape],
    options,
  );
  const report = JSON.parse(stdout);
  assert.equal(report.status, "success");
  assert.equal(report.exit_code, 0);
  assert.equal(await readFile(options.marker, "utf8"), "done");
});

test("installed SDK session closes after the shell writes exit output", async (t) => {
  const options = await fixture(t);
  const script = `
    import { createSession } from ${JSON.stringify(sdkUrl)};
    const session = await createSession({
      shell: ${JSON.stringify(options.shellPath)},
      timeoutMs: 5000,
      shutdownTimeoutMs: 3000,
    });
    try { await session.screen(); }
    finally { console.log(JSON.stringify(await session.close())); }
  `;
  const { stdout } = await exec(
    process.execPath,
    ["--input-type=module", "--eval", script],
    options,
  );
  assert.equal(JSON.parse(stdout).status, "success");
  assert.equal(await readFile(options.marker, "utf8"), "done");
});
