# Examples

Start with [quickstart.tape](quickstart.tape): Bash commands, an output assertion,
a GIF, and a screenshot. Run from a scratch directory to keep generated files
away from your project:

```sh
vhs-rs run --json /path/to/vhs-rs/examples/quickstart.tape
```

| Example | What it demonstrates |
| --- | --- |
| `readme.tape` | Drive vi, save three lines, verify the result, and capture evidence |
| `failure-demo.tape` | Expected assertion failure: the screen says 41, the tape expects 42 |
| `agent-check.tape` | Computed marker, assertion, and text evidence |
| `tui.tape` | Interactive editor input and file output |
| `styling.tape` | Themes, window bars, margins, pacing, and hidden setup |
| `theme-gallery.tape` | Built-in themes |
| `repl-driver.sh` | Live input, recording, and rendering |

## Regenerate the README assets

From the repository root, with Node 22+, Bash, and vi installed:

```sh
cargo build --locked
npm run assets --prefix npm
```

The runner executes in temporary directories, verifies the saved file and
assertions, checks screenshots and replay, and copies only the verified GIF
and intentional failure PNG into `examples/`. It removes scratch files afterward.
The editor runs with user configuration and swap/viminfo files disabled.

CI runs the same checks without changing tracked images:

```sh
node npm/scripts/test-examples.mjs /path/to/built/vhs-rs
```

Waits synchronize on terminal state. Sleeps in the showcase are for pacing the
animation, rather than deciding whether an operation completed.
