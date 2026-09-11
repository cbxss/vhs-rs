# @cbxss/vhs-rs

Typed terminal automation with a bundled Rust binary. Node 22+, Linux x64 or
macOS Apple Silicon, and a shell (bash by default). No Cargo, global installation,
install scripts, server, or daemon. Native Windows, Intel macOS and Linux ARM64
binaries are not included in this release.

```sh
npm install @cbxss/vhs-rs
npx vhs-rs demo.tape
```

The main package installs an exact-version platform package through npm optional
dependencies. Keep optional dependencies enabled. Execution works offline after
installation. Unsupported platforms and missing binary packages fail explicitly.

```ts
import { run, runFile, check, createSession, render } from "@cbxss/vhs-rs";

const report = await run({
  tape: 'Type "echo hello"\nEnter\nWait\nAssert /hello/\n',
  timeoutMs: 30_000,
});
if (report.status !== "success") console.error(report.failure);

const session = await createSession({
  cwd: process.cwd(),
  typingSpeedMs: 0,
  record: "session.jsonl",
  timeoutMs: 60_000,
});
try {
  await session.type("echo hello");
  await session.press("Enter");
  await session.waitFor(); // default shell prompt, current line
  await session.assert("hello"); // Rust regex, full screen
  console.log((await session.screen()).screen_text);
  await session.screenshot("proof.png"); // also writes proof.txt
} finally {
  await session.close();
}
await render("session.jsonl", { outputs: ["replay.gif", "final.txt"] });
```

`runFile(path, options)` reads a tape file; `check({ tape })` returns diagnostics.
`run`/`runFile` return a report even for parse errors, assertion failures, wait
timeouts and ordinary runtime errors. Transport/startup errors and cancellation
throw `VhsError`, with a stable `reason` and optional `detail`/`report`.

Session methods are `type(text, { speedMs? })`, `press(key, { count? })`,
`waitFor(pattern?, { scope?, timeoutMs? })`, `assert(pattern, options?)`,
`screen()`, `screenshot(path)`, `capture(path)`, `output(path)`, and `close()`.
Keys include `Enter`, arrows, `Escape`, and chords such as `Ctrl+C`. Chords do not
accept repeat counts. Patterns are strings in Rust regex syntax, not JavaScript
RegExp objects; inline flags such as `(?i)` are supported by the Rust engine.
A wait with a pattern defaults to full-screen matching; omitting it waits for
the prompt on the current line. Assertions default to an immediate check.

Session command failures throw `VhsError`; `detail` includes the failure screen
where available. They leave the session usable unless `strict: true`, the shell
has exited, or the session budget has expired. A non-strict session's final
report can have status `success` despite failed individual command records.

Options include `cwd`, an `env` overlay, `signal`, and `timeoutMs` in milliseconds.
The initial session configuration also accepts `shell`, `typingSpeedMs`,
`waitTimeoutMs`, `width`, `height`, `fontSize`, and a built-in `theme` name.
Width/height are canvas pixels (200–4096); font size is 8–128. Declare final
outputs with `output(path)`; declare text/golden outputs before the first action.
Full tape settings remain available through `run` and `runFile`.

Commands execute in submission order. At most 32 requests may be pending;
individual requests are limited to 1 MiB and responses to 64 MiB. `close()` is
idempotent: it rejects new work, drains accepted work, finalizes artifacts, and
waits for exit. `shutdownTimeoutMs` (default 5000) bounds finalization after the
queue drains and sets the termination grace; increase it for expensive GIFs.
Always close sessions, preferably in `finally`. Aborting the supplied signal
terminates the whole session; it cannot then be reused. A session timeout or
unexpected exit rejects outstanding work. EOF from a departed SDK owner also
interrupts a live session. Cleanup covers the shell and ordinary shell jobs;
programs that explicitly detach into another session are outside that boundary.

Recordings stream to disk, but the current Rust session also retains terminal
events and command history in memory. Use bounded sessions; disk recording does
not provide bounded memory retention. Artifacts and relative paths resolve
against `cwd`. Shell commands run with your application's permissions.

The package is ESM. Import it from TypeScript/JavaScript in Node; CommonJS callers
can use dynamic `import()`. The `render` result lists absolute output paths; a
failed render throws and may have written partial artifacts.
