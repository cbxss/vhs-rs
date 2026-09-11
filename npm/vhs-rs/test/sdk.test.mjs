import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, readFile, writeFile, rm, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { spawn } from "node:child_process";
const root = process.env.VHS_TEST_PACKAGE_ROOT;
if (!root)
  throw new Error(
    "Run npm run test:package after staging the release artifacts",
  );
const dist = join(root, "node_modules/@cbxss/vhs-rs/dist");
const { run, runFile, check, createSession, render, VhsError } = await import(
  pathToFileURL(join(dist, "index.js"))
);
const { resolveBinary, version } = await import(
  pathToFileURL(join(dist, "binary.js"))
);

async function scratch(t) {
  const dir = await mkdtemp(join(tmpdir(), "vhs-sdk-"));
  t.after(() => rm(dir, { recursive: true, force: true }));
  return dir;
}
async function session(t, options = {}) {
  const s = await createSession({
    cwd: await scratch(t),
    typingSpeedMs: 0,
    ...options,
  });
  t.after(async () => {
    try {
      await s.close();
    } catch {}
  });
  return s;
}
const reason = (expected) => (error) =>
  error instanceof VhsError && error.reason === expected;

test("packed resolver, supported platforms and versions", () => {
  assert.ok(resolveBinary().startsWith(root));
  assert.throws(
    () => resolveBinary("win32", "x64"),
    reason("unsupported_platform"),
  );
  assert.throws(
    () => resolveBinary("linux", "arm64"),
    reason("unsupported_platform"),
  );
  assert.throws(
    () =>
      resolveBinary(
        process.platform === "linux" ? "darwin" : "linux",
        process.platform === "linux" ? "arm64" : "x64",
      ),
    reason("missing_binary"),
  );
  assert.equal(version, "0.3.0");
});

test("batch, file paths, cwd, env, check and all structured failure codes", async (t) => {
  const cwd = await scratch(t);
  const tape =
    'Set TypingSpeed 0ms\nType "echo $SDK_TEST"\nEnter\nWait\nAssert /from-env/\n';
  const good = await run({ tape, cwd, env: { SDK_TEST: "from-env" } });
  assert.equal(good.status, "success");
  await writeFile(join(cwd, "-a tape.tape"), tape);
  assert.equal(
    (await runFile("-a tape.tape", { cwd, env: { SDK_TEST: "from-env" } }))
      .exit_code,
    0,
  );
  assert.equal((await check({ tape: "Screen", cwd })).ok, true);
  assert.equal((await check({ tape: "BadCommand", cwd })).ok, false);
  const bad = await run({ tape: "BadCommand", cwd });
  assert.equal(bad.status, "parse_error");
  assert.ok(bad.errors.length);
  assert.equal(
    (await run({ tape: "Assert /definitely-missing-token/", cwd })).exit_code,
    1,
  );
  assert.equal(
    (await run({ tape: "Wait@10ms /definitely-missing-token/", cwd }))
      .exit_code,
    3,
  );
  assert.equal((await runFile("absent.tape", { cwd })).exit_code, 4);
  const timeout = await run({ tape: "Sleep 30s", cwd, timeoutMs: 200 });
  assert.equal(timeout.failure.reason, "run_timeout");
});

test("live state, FIFO commands, exact text transport, artifacts and replay", async (t) => {
  const cwd = await scratch(t);
  const s = await session(t, {
    cwd,
    record: "timeline.jsonl",
    width: 600,
    height: 400,
  });
  await s.output("final.png");
  const text = "printf '%s\\n' 'quotes \" slash \\ unicode 雪'";
  await Promise.all([s.type(text), s.press("Enter"), s.waitFor()]);
  await s.assert('quotes " slash \\\\ unicode 雪');
  assert.match((await s.screen()).screen_text, /unicode 雪/);
  // Newlines are decoded as text, not additional protocol requests.
  await s.type("echo multi-one\necho multi-two\n");
  await s.waitFor("multi-two");
  await s.screenshot('a "quoted" proof.png');
  await s.capture("capture.txt");
  const first = s.close();
  assert.equal(first, s.close());
  const report = await first;
  assert.equal(report.status, "success");
  assert.ok(report.artifacts.some((a) => a.path === "final.png"));
  assert.ok((await stat(join(cwd, 'a "quoted" proof.png'))).size > 0);
  assert.match(await readFile(join(cwd, "capture.txt"), "utf8"), /multi-two/);
  await assert.rejects(s.type("after close"), reason("session_closed"));
  const rendered = await render("timeline.jsonl", {
    cwd,
    outputs: ["replay.gif", "replay.txt", "replay.png"],
    idleLimitMs: 20,
  });
  assert.deepEqual(
    rendered.paths,
    ["replay.gif", "replay.txt", "replay.png"].map((p) => resolve(cwd, p)),
  );
  assert.equal(
    (await readFile(join(cwd, "replay.gif"))).subarray(0, 3).toString(),
    "GIF",
  );
  await assert.rejects(
    render("missing.jsonl", { cwd, outputs: ["bad.gif"] }),
    reason("render_failed"),
  );
});

test("recoverable errors, invalid input and queue limits", async (t) => {
  const s = await session(t);
  await assert.rejects(
    s.assert("never-matches-584093"),
    (e) =>
      e.reason === "assert_failed" && typeof e.detail.screen_text === "string",
  );
  await assert.rejects(
    s.waitFor("never-matches-584093", { timeoutMs: 10 }),
    reason("wait_timeout"),
  );
  await assert.rejects(s.waitFor("["), reason("invalid_request"));
  await assert.rejects(
    s.type("x", { speedMs: -1 }),
    reason("invalid_argument"),
  );
  await assert.rejects(s.press("Screen"), reason("invalid_request"));
  await assert.rejects(
    s.type("x".repeat(1024 * 1024)),
    reason("invalid_argument"),
  );
  await s.type("echo recovered");
  await s.press("Enter");
  await s.waitFor();
  assert.match((await s.screen()).screen_text, /recovered/);
  const accepted = Array.from({ length: 32 }, () => s.screen());
  await assert.rejects(s.screen(), reason("queue_full"));
  const closing = s.close();
  await Promise.all(accepted);
  await closing;
});

test("concurrent sessions remain independent", async (t) => {
  const [a, b] = await Promise.all([session(t), session(t)]);
  await Promise.all([
    a.type("echo SESSION_ALPHA"),
    b.type("echo SESSION_BETA"),
  ]);
  await Promise.all([a.press("Enter"), b.press("Enter")]);
  await Promise.all([a.waitFor(), b.waitFor()]);
  assert.doesNotMatch((await a.screen()).screen_text, /SESSION_BETA/);
  assert.doesNotMatch((await b.screen()).screen_text, /SESSION_ALPHA/);
});

test("strict failure, unexpected shell exit, pre-abort and invalid startup", async (t) => {
  const s = await session(t, { strict: true });
  const pending = [s.assert("never-matches-92843"), s.screen()];
  const results = await Promise.allSettled(pending);
  assert.equal(results[0].status, "rejected");
  assert.equal(results[1].status, "rejected");
  const exited = await session(t);
  await exited.type("exit");
  await exited.press("Enter");
  await assert.rejects(exited.waitFor("unreachable"), (e) =>
    ["child_exited", "wait_timeout"].includes(e.reason),
  );
  const abort = new AbortController();
  abort.abort();
  await assert.rejects(
    createSession({ signal: abort.signal }),
    reason("aborted"),
  );
  await assert.rejects(
    createSession({ width: 65535 }),
    reason("invalid_request"),
  );
});

test("abort interrupts a blocked wait and cleans ordinary shell jobs", async (t) => {
  const cwd = await scratch(t);
  const controller = new AbortController();
  const s = await session(t, { cwd, signal: controller.signal });
  await s.type(
    "echo $$ > shell.pid; sleep 300 & echo $! > background.pid; sh -c 'echo $$ > foreground.pid; exec sleep 300'",
  );
  await s.press("Enter");
  // Files prove both jobs were launched before cancellation.
  for (let i = 0; i < 100; i++) {
    try {
      await stat(join(cwd, "foreground.pid"));
      break;
    } catch {
      await new Promise((r) => setTimeout(r, 10));
    }
  }
  const pid = Number(await readFile(join(cwd, "shell.pid"), "utf8"));
  const background = Number(
    await readFile(join(cwd, "background.pid"), "utf8"),
  );
  const foreground = Number(
    await readFile(join(cwd, "foreground.pid"), "utf8"),
  );
  t.after(() => {
    for (const id of [pid, background, foreground]) {
      try {
        process.kill(id, "SIGTERM");
      } catch {}
    }
  });
  const waiting = s.waitFor("cannot-appear", { timeoutMs: 60000 });
  setTimeout(() => controller.abort(), 30);
  await assert.rejects(waiting, (e) =>
    ["aborted", "interrupted"].includes(e.reason),
  );
  await assert.rejects(s.close());
  assert.throws(() => process.kill(pid, 0));
  // A dead orphan can briefly be a zombie awaiting the host's init reap.
  const alive = async (pid) => {
    try {
      process.kill(pid, 0);
    } catch {
      return false;
    }
    if (process.platform === "linux") {
      try {
        return !(await readFile(`/proc/${pid}/stat`, "utf8")).includes(") Z ");
      } catch {
        return false;
      }
    }
    return true;
  };
  for (let i = 0; i < 100 && (await alive(background)); i++)
    await new Promise((r) => setTimeout(r, 10));
  assert.equal(
    await alive(background),
    false,
    `background process ${background} survived`,
  );
  assert.equal(
    await alive(foreground),
    false,
    `foreground process ${foreground} survived`,
  );
});

test("batch cancellation and session budget", async (t) => {
  const cwd = await scratch(t);
  const controller = new AbortController();
  const promise = run({ tape: "Sleep 60s", cwd, signal: controller.signal });
  setTimeout(() => controller.abort(), 100);
  await assert.rejects(promise, reason("aborted"));
  const s = await session(t, { timeoutMs: 500 });
  await assert.rejects(s.waitFor("never-matches", { timeoutMs: 60000 }), (e) =>
    ["run_timeout", "process_exited"].includes(e.reason),
  );
});

function raw() {
  const child = spawn(resolveBinary(), ["repl", "--json-input", "--quiet"], {
    stdio: "pipe",
  });
  let buffer = "";
  const lines = [];
  const waiters = [];
  child.stdout.setEncoding("utf8");
  child.stdout.on("data", (chunk) => {
    buffer += chunk;
    let end;
    while ((end = buffer.indexOf("\n")) >= 0) {
      const value = JSON.parse(buffer.slice(0, end));
      buffer = buffer.slice(end + 1);
      const waiter = waiters.shift();
      if (waiter) waiter(value);
      else lines.push(value);
    }
  });
  const done = new Promise((resolve) => child.on("close", resolve));
  return {
    child,
    done,
    next: () =>
      lines.length
        ? Promise.resolve(lines.shift())
        : new Promise((resolve) => waiters.push(resolve)),
    send: (value) => child.stdin.write(JSON.stringify(value) + "\n"),
  };
}
test(
  "wire protocol validates IDs and commands atomically and closes once",
  { timeout: 15000 },
  async (t) => {
    const p = raw();
    t.after(() => p.child.kill("SIGKILL"));
    assert.equal((await p.next()).binary_version, version);
    p.send({ id: 1, command: { op: "type", text: "echo hi", surprise: true } });
    assert.equal((await p.next()).failure.reason, "invalid_request");
    p.send({
      id: 2,
      command: { op: "configure", settings: { typing_speed_ms: 0 } },
    });
    assert.equal((await p.next()).status, "ok");
    p.send({ id: 2, command: { op: "screen" } });
    assert.equal((await p.next()).failure.reason, "invalid_request");
    p.send({ id: 3, command: { op: "screen" } });
    const screen = await p.next();
    assert.equal(screen.id, 3);
    assert.equal(screen.status, "ok");
    assert.doesNotMatch(screen.detail.screen_text, /echo hi/);
    p.send({ id: 4, command: { op: "close" } });
    assert.equal((await p.next()).report.status, "success");
    assert.equal(await p.done, 0);
  },
);
test("EOF interrupts an active wire request", { timeout: 10000 }, async (t) => {
  const p = raw();
  t.after(() => p.child.kill("SIGKILL"));
  await p.next();
  p.send({ id: 1, command: { op: "screen" } });
  await p.next();
  p.send({
    id: 2,
    command: { op: "wait", pattern: "never-matches", timeout_ms: 60000 },
  });
  await new Promise((r) => setTimeout(r, 50));
  p.child.stdin.end();
  const result = await p.next();
  assert.equal(result.id, 2);
  assert.equal(result.failure.reason, "interrupted");
  assert.equal(await p.done, 4);
});
