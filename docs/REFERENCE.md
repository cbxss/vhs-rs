# Reference

[Quickstart and examples](../README.md) · [TypeScript SDK](../npm/vhs-rs/README.md)

## For agents

This is the part built for scripts and AI agents driving vhs-rs in a loop.
The exit codes, JSON shape, forensics naming, and determinism rules below are
stable API.

`vhs-rs run --json` prints one JSON object on stdout: a record per command
(with line numbers and timing), every artifact written, and on failure the
exact screen text at the moment the check ran — so the caller can see *why* a
pattern missed without opening a single file.

```jsonc
{
  "version": 1,
  "tape": "demo.tape",
  "status": "assert_failed",        // success | assert_failed | parse_error
  "exit_code": 1,                   //   | wait_timeout | runtime_error
  "term": { "cols": 77, "rows": 21, "shell": "bash" },
  "commands": [
    { "index": 3, "line": 9, "col": 1, "command": "Wait Line",
      "status": "ok", "elapsed_ms": 132 },
    // `command` is the rendered form; the tape syntax for this line is
    // `Assert+Screen /status: 42/`.
    { "index": 4, "line": 11, "col": 1, "command": "Assert Screen status: 42",
      "status": "failed",
      "detail": { "matched": false, "regex": "status: 42", "scope": "Screen",
                  "screen_text": "> echo status: 41\nstatus: 41\n>" } },
    { "index": 5, "line": 12, "col": 1, "command": "Screenshot proof.png",
      "status": "skipped" }          // commands after a failure are skipped
  ],
  "failure": { "command_index": 4, "reason": "assert_failed",
               "message": "Assert /status: 42/ did not match Screen" },
  "artifacts": [
    { "path": "demo.failure.txt", "kind": "failure_text" },
    { "path": "demo.failure.png", "kind": "failure_png" }
  ]
}
```

Details an agent can rely on:

- `run --json` prints a run report on **every** exit path, parse errors
  included (`status: "parse_error"`, plus a top-level `errors` array with
  `{line, col, message}` diagnostics) — one schema to parse, keyed on
  `status`. Even SIGINT/SIGTERM produce a final (partial) report.
- `failure.reason` matches `status` except when a more specific cause is
  known: `child_exited` (the shell died — a longer timeout won't help),
  `run_timeout` (`--timeout` expired), or `interrupted` (signal).
- `vhs-rs run --timeout 60s …` puts a wall-clock budget on the whole run:
  on expiry it exits 4 with reason `run_timeout` — and still writes the
  report and failure forensics. Prefer this over killing the process.
- On a failed Wait/Assert, `detail.screen_text` is always the **full**
  screen; Line-scoped checks also carry `detail.line_text`, the single line
  the regex ran against.
- `Screen` is the cheap readback command: it drains the PTY and returns
  `{screen_text, cursor:{col,row,visible}}` in that command's `detail`.
- Tapes read from stdin (`vhs-rs run -`) name their forensics
  `stdin.failure.txt` / `stdin.failure.png`.

### Driving vhs-rs live

`vhs-rs repl` keeps one PTY alive while stdin supplies one tape-language line
at a time. Stdout is newline-delimited JSON only, flushed after every response;
warnings stay on stderr. The first line is `{"kind":"ready","version":1}`.
The shell spawns lazily on the first action command, then a one-time
`{"kind":"term","cols":77,"rows":21,"shell":"bash"}` follows.

Each input line produces at least one response:

```jsonc
{"kind":"command","index":0,"input_line":1,"command":"Type echo hi","status":"ok","elapsed_ms":512}
{"kind":"empty","input_line":2}
{"kind":"parse_error","input_line":3,"errors":[{"line":1,"col":1,"message":"Invalid command: Foo"}]}
{"kind":"report", "version":1, "status":"success", "exit_code":0, ...}
```

Multi-command lines emit one `command` response per command, with a
session-monotonic `index`. A line with parse errors is atomic: nothing from
that line runs. Failed commands do **not** abort the REPL unless `--strict` is
set; the agent can inspect the failure response and choose the next command.
Use `--timeout 60s` as the session budget and prefer bounded waits like
`Wait@2s /ready/`: a blocking `Wait` blocks the REPL until it matches or times
out. Long sessions keep the in-memory event log, so the intended long-running
workflow is `vhs-rs repl --record session.jsonl` followed by `vhs-rs render`;
bounded retention is a future extension, not part of protocol v1.

### Record now, render later

`--record` on `run` or `repl` streams a native JSONL timeline as the session
runs. `Output x.jsonl` does the same from a tape. The first line is a header
with replay metadata:

```jsonc
{"kind":"header","version":1,"cols":77,"rows":21,"shell":"bash","tape":"demo.tape",
 "theme":{...},"render":{...},"cursor_blink":true,
 "max_fps":50.0,"playback_speed":1.0,"loop_offset":null}
```

Following lines are append-only events with microsecond timestamps:

```jsonc
{"kind":"output","t_us":1234,"data":"..."}
{"kind":"resize","t_us":2000,"cols":100,"rows":30}
{"kind":"visibility","t_us":3000,"visible":false}
{"kind":"theme","t_us":4000,"theme":{...}}
{"kind":"command","t_us":5000,"index":3,"line":9,"command":"Wait Line","status":"ok","elapsed_ms":132}
{"kind":"exit","t_us":6000}
```

Render without re-running:

```sh
vhs-rs render session.jsonl -o replay.gif -o final.png -o final.txt
vhs-rs render session.cast -o replay.gif --theme Dracula --idle-limit 2s --speed 1.5
```

The recorded grid is authoritative. Output dimensions are re-derived from the
grid, font, and render options, so changing `--theme` or `--font-size` changes
pixels without changing terminal content. `--idle-limit` caps long quiet gaps,
and hidden spans are cut the same way VHS cuts `Hide`/`Show` sections.

`vhs-rs check --json` prints `{"ok", "commands", "errors": [{"line", "col", "message"}]}`.

Two runs of the same tape produce byte-identical `.txt` artifacts — diff them
and you have a regression test (this repo's own test suite does exactly that,
see [`tests/golden.rs`](../tests/golden.rs)). vhs-rs makes this hold by pinning
the shell (`bash --noprofile --norc -i`), the prompts (`PS1="> "`, which the
default `Wait` pattern matches, and `PS2="... "`, which deliberately doesn't —
a shell stuck on an unclosed quote times out instead of passing), `TERM`, and
the locale, and by waiting for the prompt before the first keystroke. It also sets `VHS_RS=1` in the child so
your scripts can tell they're being recorded.

One caveat: everything else in the environment is inherited. If a command's
output depends on some variable, pin it in the tape:

```elixir
Env LS_COLORS "di=01;34:ex=01;32"
```

## The tape language

vhs-rs supports VHS-style tapes. Use `vhs-rs check` to identify unsupported
commands or settings before execution. Rendering and behavior can differ; the
following reference describes vhs-rs's supported commands and limits.

### Supported

`Type`, every key (`Enter`, `Tab`, `Backspace`, arrows, `PageUp`/`PageDown`,
`Home`/`End`, …) with `[@speed] [count]`, chords (`Ctrl+Shift+O`, `Alt+.`),
`Sleep`, `Wait[+Line|+Screen][@timeout] [/re/]`, `Screen`, `Screenshot`,
`Hide`/`Show`, `Require`, `Env`, `Source`, `Copy`/`Paste`,
`Output .gif/.png/.txt/.ascii/.test/.cast/.jsonl`, and `Set` for the full VHS
settings list. Arrow keys respect application cursor mode, so vim and fzf
behave.

### New

| command | what it does |
| --- | --- |
| `Assert[+Screen\|+Line][@timeout] /re/` | fail the run (exit 1) if the pattern isn't on screen; with `@timeout` it retries until the deadline |
| `Capture x.txt` | dump the screen as plain text, right now |
| `Screen` | return the current screen text and cursor in JSON command detail |
| `Output x.jsonl` | native vhs-rs timeline for later `render` |
| `Output x.cast` | asciicast v3 event log ([asciinema](https://asciinema.org) format) |
| `Output x.png` | final frame as a PNG |

### Different or dropped

- `Output x.mp4` / `x.webm` — dropped; video needs ffmpeg, which is the kind
  of dependency this project exists to not have. Make a `.gif` or `.cast`.
- `Copy`/`Paste` use an internal clipboard, never the system one.
- `Set FontFamily` is accepted but ignored — the font is embedded
  (JetBrains Mono, with a Nerd Font symbols fallback so powerline glyphs work).
- `ScrollUp`/`ScrollDown` send real mouse-wheel events when the program has
  enabled mouse reporting (vim, htop, fzf); otherwise they warn and do nothing.

### Things that will bite you exactly once

- Quote absolute paths: `/` starts a regex, so it's `Screenshot "/tmp/x.png"`.
- `Wait` defaults to scope `Line` (the cursor's row — right for "is the prompt
  back"). `Assert` defaults to `Screen` (right for "did it print anywhere").
- Strings have no escape sequences. `Type "printf '\n'"` sends a literal
  backslash-n to the shell, which is usually what you want.
- After the first action command, only `TypingSpeed`, `WaitTimeout`,
  `WaitPattern`, `PlaybackSpeed`, and `Theme` may be `Set` — the terminal's
  geometry is fixed once the shell spawns. VHS silently ignores late setting
  changes; vhs-rs makes them a `check` error.

### Settings

Defaults match VHS: 1200×600 canvas, padding 60, font size 22, typing speed
50ms, wait timeout 15s, wait pattern `>$`. Plus `Theme` (348 built-in names or
inline JSON), `WindowBar` (`Colorful`, `ColorfulRight`, `Rings`, `RingsRight`),
`BorderRadius`, `Margin`/`MarginFill`, `LetterSpacing`, `LineHeight`,
`Framerate` (GIF max fps, capped at 50), `PlaybackSpeed`, `LoopOffset`,
`CursorBlink` (on by default), and `Shell` (`bash`, `sh`, `zsh`, `fish` — each
spawned with its no-config flags).

## How is this different from VHS?

VHS renders through Chromium, ttyd, and ffmpeg. It also supports integration
testing using text/golden output. vhs-rs uses a native terminal model and renderer
and adds explicit assertions, structured reports, and failure artifacts.

vhs-rs keeps the tape language and does the terminal work itself:

|  | VHS | vhs-rs |
| --- | --- | --- |
| runtime deps | Chromium, ttyd, ffmpeg | chosen shell (Bash by default) |
| `Wait` | polls via JS eval | event-driven, re-checked per output chunk |
| assertions | — | `Assert`, exit code 1 |
| machine output | prose on stderr | `--json` report |
| failure artifacts | — | screen dumped as `.txt` + `.png`, always |
| `.mp4`/`.webm` | yes | no |

