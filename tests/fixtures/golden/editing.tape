# editing.tape — multi-command line editing: fix a typo with Backspace, then
# cancel a half-typed command with Ctrl+C. All output is self-generated, so
# the golden is machine-independent.

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
# Ctrl+C settles for one TypingSpeed; slow it down so the ^C echo and the new
# prompt are on screen before the golden frame is recorded.
Set TypingSpeed 300ms
Ctrl+C
Set TypingSpeed 10ms
Wait

Type "echo done"
Enter
Wait

Assert /done/
