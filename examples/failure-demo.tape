# failure-demo.tape — a deliberately failing Assert, to demonstrate vhs-rs's
# failure forensics. This tape is SUPPOSED to fail.
#
#   vhs-rs run --json examples/failure-demo.tape ; echo "exit: $?"
#
# Expected outcome:
#   * exit code 1 (assert failure)
#   * failure-demo.failure.txt — the exact screen text at the moment of failure
#   * failure-demo.failure.png — the same screen, rendered
#   * with --json: the report's `failure` object names the failing command,
#     and the Assert's `detail.screen_text` carries what was actually shown.
#
# Forensics files are written next to the first Output path automatically on
# ANY failure (assert, wait timeout, runtime error) — no flags needed.

Output failure-demo.txt

Set Width 700
Set Height 420
Set FontSize 17
Set Padding 18
Set Theme "Catppuccin Mocha"
Set WindowBar Colorful
Set BorderRadius 8
Set Margin 14
Set MarginFill "#7287fd"
Set TypingSpeed 20ms

Type "echo the answer is 41"
Enter
Wait

# Wrong on purpose: the screen says 41, we claim 42.
Assert /(?m)^the answer is 42$/
