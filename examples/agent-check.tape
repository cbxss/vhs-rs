# agent-check.tape — the agent workflow: run a command, wait for the prompt,
# assert on the rendered screen, and capture plain-text evidence an agent can
# read without vision.
#
#   vhs-rs run --json examples/agent-check.tape
#
# Exit codes: 0 success, 1 assert failed, 2 parse error, 3 wait timeout,
# 4 runtime error. On any failure vhs-rs writes vhs-rs-failure.txt/.png.

Output agent-check.txt

Require bash

Type "echo vhs-rs-status-$((6*7))"
Enter
Wait

# The screen must show the computed marker (proves the shell really ran it).
Assert+Screen /vhs-rs-status-42/

# Dump the screen as plain text: the agent diffs this against a golden file.
Capture agent-check-screen.txt
