# vhs-rs

**Script, test, and record your terminal. One binary.**

Run commands and interact with terminal apps, check what appears on screen,
and save GIFs, screenshots, or replayable recordings. Use a `.tape` script or
drive a live session from TypeScript. No browser, ttyd, or ffmpeg required.

<img alt="vhs-rs edits a file in vi, verifies the saved result, and captures a screenshot" src="https://raw.githubusercontent.com/cbxss/vhs-rs/main/examples/readme.gif" width="700" />

[![CI](https://github.com/cbxss/vhs-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/cbxss/vhs-rs/actions/workflows/ci.yml)
[![npm](https://img.shields.io/npm/v/@cbxss/vhs-rs)](https://www.npmjs.com/package/@cbxss/vhs-rs)
[![crates.io](https://img.shields.io/crates/v/vhs-rs)](https://crates.io/crates/vhs-rs)

[Showcase source](https://github.com/cbxss/vhs-rs/blob/main/examples/readme.tape) ·
[More examples](https://github.com/cbxss/vhs-rs/tree/main/examples)

## Install

**Linux x64 or macOS Apple Silicon:**

```sh
curl -fsSL https://raw.githubusercontent.com/cbxss/vhs-rs/main/install.sh | sh
```

The installer writes to `~/.local/bin`; add it to your `PATH` if needed.
Alternatively, use npm with **Node 22+**:

```sh
npm install @cbxss/vhs-rs
npx vhs-rs --version
```

Rust users can install from source:

```sh
cargo install --locked vhs-rs
```

Running needs a shell, Bash by default. Prebuilt binaries cover Linux x64 and
macOS ARM64. Other Unix targets require a source build; native Windows is not
supported. [Release downloads](https://github.com/cbxss/vhs-rs/releases) are also available.

## Try it

Save this as `demo.tape`. It computes a value, checks the result, and produces
a GIF and screenshot:

```elixir
Output demo.gif
Set Theme "Catppuccin Mocha"
Set WindowBar Colorful

Type "echo $((6 * 7))"
Enter
Wait
Assert /(?m)^42$/
Screenshot proof.png
Sleep 1s
```

```sh
vhs-rs demo.tape
# Or, with the npm package:
npx vhs-rs demo.tape
```

You get `demo.gif`, `proof.png`, and `proof.txt`. The assertion checks an output
line containing exactly `42`; the typed expression does not contain the result.

## Record a demo

Tapes are plain text: type commands, press keys, wait for output, and capture
the terminal. Themes, window bars, timing, and hidden setup let you control
the presentation.

```sh
vhs-rs run --record session.jsonl demo.tape
vhs-rs render session.jsonl -o replay.gif -o final.png -o final.txt
```

Recording and rendering are separate, so you can replay a session with a new
theme or speed without running its commands again. Existing asciicast v2/v3
recordings can also be rendered. Outputs include GIF, PNG, text, and asciicast.

## Test terminal behavior

`Wait` waits for a screen pattern or the shell prompt. `Assert` fails the tape
when a screen pattern does not match. Request JSON for command results and
failure evidence:

```sh
vhs-rs run --json --timeout 30s demo.tape
```

On failure, vhs-rs saves the final screen as `<stem>.failure.png` and
`<stem>.failure.txt`. This example deliberately expects `42` while the program
prints `41`, and exits with code `1`:

<img alt="An intentional assertion failure: the terminal prints 41 while the tape expects 42" src="https://raw.githubusercontent.com/cbxss/vhs-rs/main/examples/failure-forensics.png" width="700" />

[Run the failure example](https://github.com/cbxss/vhs-rs/blob/main/examples/failure-demo.tape).

| Exit code | Meaning |
| --- | --- |
| 0 | Tape completed successfully |
| 1 | Assertion failed |
| 2 | Parse or validation error |
| 3 | Wait timed out |
| 4 | Runtime, I/O, PTY, interruption, or run-budget error |

**Screen assertions inspect visible text, including echoed input and earlier
output.** `Assert /hello/` can match the command `echo hello` itself. Use an
anchored output line such as `Assert /(?m)^hello$/`, a computed result, or a
specific screen state. Clear stale output when it could satisfy a later check.

`Wait` returning means its pattern matched. Tape success is not a check of each
shell command's exit status: `false` followed by `Wait` can complete successfully.
To check a command's status, print it immediately and assert on the output:

```elixir
Type "false; printf 'command-status:%s\n' $?"
Enter
Wait
Assert /(?m)^command-status:0$/
```

This fails as intended because the command prints `command-status:1`.

## Control a terminal from TypeScript

The npm package includes its native binary. Installation does not require
Cargo, global tools, a server, or a daemon; execution works offline afterward.

```ts
import { createSession } from "@cbxss/vhs-rs";

const session = await createSession({ typingSpeedMs: 0, timeoutMs: 30_000 });
try {
  await session.type("echo $((6 * 7))");
  await session.press("Enter");
  await session.waitFor();
  await session.assert("(?m)^42$");
  console.log((await session.screen()).screen_text);
  await session.screenshot("proof.png");
} finally {
  await session.close();
}
```

The same interface works for automation scripts, test runners, and coding agents.
The CLI also supports stdin tapes and a persistent JSON REPL.

## Capabilities and limits

- Real PTY input, keys and chords, screen reads, regex waits and assertions.
- Native GIF/PNG rendering, text captures, JSON reports, and recording/replay.
- VHS-style `.tape` syntax, embedded JetBrains Mono and Nerd Font symbols,
  and 348 built-in themes.
- GIF and PNG output; MP4/WebM output and custom font selection are unavailable.
- Screen behavior depends on the terminal emulator. Validate the apps you
  intend to automate; VHS syntax support does not imply identical rendering.
- Session history remains in memory even when recording to disk. Use bounded
  sessions; disk recording does not provide bounded memory retention.

## Documentation and examples

- [Tape, CLI, JSON reports, REPL, and replay reference](https://github.com/cbxss/vhs-rs/blob/main/docs/REFERENCE.md)
- [TypeScript API and lifecycle](https://github.com/cbxss/vhs-rs/blob/main/npm/vhs-rs/README.md)
- [JSON wire protocol](https://github.com/cbxss/vhs-rs/blob/main/npm/PROTOCOL.md)
- [Examples and asset regeneration](https://github.com/cbxss/vhs-rs/tree/main/examples)
- [Build and release instructions](https://github.com/cbxss/vhs-rs/blob/main/npm/RELEASING.md)

## Credits

Built on [VHS](https://github.com/charmbracelet/vhs)'s tape language and theme
catalog, [asciinema](https://asciinema.org)'s
[avt](https://crates.io/crates/avt) terminal emulator and recording format,
[JetBrains Mono](https://www.jetbrains.com/lp/mono/), and
[Nerd Fonts](https://www.nerdfonts.com/) symbols. Font licenses are included.

MIT licensed.
