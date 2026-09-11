# Structured session protocol v1

Start `vhs-rs repl --json-input [--record path] [--timeout 60s] [--strict]`.
All stdout is UTF-8 newline-delimited JSON. Diagnostics go to stderr.
The first response is:

```json
{"kind":"ready","version":1,"binary_version":"0.3.1"}
```

Requests have exactly `id` and `command`. IDs are strictly increasing positive
JavaScript-safe integers. The request must fit in 1 MiB including its newline.
There are at most eight buffered requests inside Rust; the SDK limits pending
calls to 32. Commands execute serially and preserve the existing tape semantics.

```json
{"id":1,"command":{"op":"configure","settings":{"typing_speed_ms":0}}}
{"id":2,"command":{"op":"type","text":"echo hello"}}
{"id":3,"command":{"op":"press","key":"Enter"}}
{"id":4,"command":{"op":"wait"}}
{"id":5,"command":{"op":"screen"}}
{"id":6,"command":{"op":"close"}}
```

Each processed request produces one `{"kind":"result","id":…,"status":"ok"}`
or `status:"failed"` with `failure:{reason,message}`. Command results include
`events`, the existing REPL command/terminal records, and `detail` when present.
`screen` detail is `{screen_text,cursor:{col,row,visible}}`. A failed match's
detail includes the screen evidence. A successful close includes the final
`report`; a failed finalization includes both `failure` and `report`.

Operations:

| op | fields |
| --- | --- |
| configure | settings: shell, typing_speed_ms, wait_timeout_ms, width, height, font_size, theme (all optional) |
| type | text; optional speed_ms |
| press | key; optional count (1–65535, non-chord keys only) |
| wait | optional pattern, scope, timeout_ms |
| assert | pattern; optional scope, timeout_ms |
| screen | none |
| screenshot / capture / output | path |
| close | none |

Fields are validated before execution; unknown fields/operations are errors.
Text and paths are JSON strings, not tape source. Match patterns use Rust regex
syntax. Scope is `line` or `screen`; waits without a pattern default to the
current line/default prompt, other matches default to screen. Durations are
unsigned 32-bit integer milliseconds. Configuration must precede actions;
width/height are 200–4096 canvas pixels, font size 8–128, theme a built-in name.
Configuration is validated as a whole before applying any settings.

Recoverable command errors permit more requests unless strict mode is enabled.
Timeout, signal, disconnect and strict failure terminate the session and emit a
final `kind:"report"` when stdout remains writable. Clients must reject queued
requests that did not receive results when a terminal report or process exit
arrives. Malformed JSON without a usable ID gets an error with `id:null`.
IDs on invalid, successfully decoded requests are consumed; malformed request
schemas need not consume an ID.

Use `close` to drain work and finalize artifacts. EOF indicates loss of the
client and interrupts active work, so do not pipe a finite request list and
close stdin while awaiting results. SIGTERM/SIGINT likewise interrupt active
waits. Synchronous rendering can delay signal handling; the SDK has an outer
termination watchdog. Terminal state/history retention is currently unbounded
inside a session; use session budgets and periodically close/reopen sessions.

The tape-language REPL remains the default, with its existing protocol unchanged.
