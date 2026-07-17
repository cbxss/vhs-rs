# editing.tape — multi-command line editing: fix a typo with Backspace, then
# abandon a half-typed command with Ctrl+U (kill line). All output is
# self-generated, so the golden is machine-independent. (Ctrl+C would read
# better, but bash 5 echoes `^C` where bash 3.2 — macOS — echoes nothing,
# and this golden must be byte-identical on both.)

Output editing.txt

Set TypingSpeed 10ms
Set WaitTimeout 10s

Type "echo edit-wrong"
Backspace 5
Type "right"
Enter
Wait

Assert /edit-right/

Type "this will never run"
Ctrl+U
Wait

Type "echo done"
Enter
Wait

Assert /done/
