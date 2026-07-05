# failure-demo.tape — a deliberately failing Assert, to demonstrate vterm's
# failure forensics. This tape is SUPPOSED to fail.
#
#   vterm run --json examples/failure-demo.tape ; echo "exit: $?"
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

Set TypingSpeed 20ms

Type "echo the answer is 41"
Enter
Wait

# Wrong on purpose: the screen says 41, we claim 42.
Assert /the answer is 42/
