# Launch draft: vhs-rs 0.3.2

## Short post

I built vhs-rs to script, test, and record real terminals. It runs VHS-style
tapes or live TypeScript sessions, produces GIFs and screenshots, and saves
screen evidence when an assertion fails. Recording and replay work without a
browser, ttyd, or ffmpeg.

The demo edits a file in vi, saves it, checks the result, and captures the screen:

![vhs-rs driving vi](https://raw.githubusercontent.com/cbxss/vhs-rs/main/examples/readme.gif)

Three ways to use it:

- Record a terminal workflow as a GIF or replayable session.
- Test CLI/TUI behavior with waits, assertions, and failure screenshots.
- Control a persistent terminal from TypeScript, including coding-agent workflows.

Prebuilt binaries support Linux x64 and Apple Silicon; Node users can install
`@cbxss/vhs-rs` without a Rust toolchain. A shell is required at runtime.

Code, examples, and installation: https://github.com/cbxss/vhs-rs

I'd like feedback from anyone who maintains a terminal app or automates terminal
workflows. What would make this useful in your own project?

## Release notes

- A practical quickstart and separate paths for recording, testing, and TypeScript control.
- A verified editor showcase and an intentional assertion-failure screenshot.
- Clear guidance on echoed input, stale screen contents, and shell exit statuses.
- Updated package discovery metadata and coordinated version 0.3.2.
- Automated crates.io publishing with source/toolchain/checksum receipts and safe retries.
- Fresh installation checks for npm, the GitHub installer, and Cargo on Linux and macOS.

## Sharing checklist

Use the verified release and working GIF. Adapt the draft to the community's
rules, identify yourself as the author, and ask for specific feedback. Track
repository traffic and install attempts alongside stars. This file is draft
copy; generating it does not publish a post.
