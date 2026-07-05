# tui.tape — drive a full-screen TUI (vi) the way an agent would: enter the
# editor, wait for its screen to paint, type, save, and verify the result
# back at the shell.
#
#   vhs-rs run examples/tui.tape
#
# Requires `vi` (any vim/nvi) on PATH.

Output tui.gif

Require vi

Set TypingSpeed 25ms

# Work in a throwaway directory so the run never touches real files.
Hide
Type `cd "$(mktemp -d)"`
Enter
Wait
Show

Type "vi notes.txt"
Enter
# vi has no shell prompt — wait for its status line to appear on screen.
Wait+Screen@10s /notes\.txt/

# Insert mode, write a line, back to normal mode.
Type "i"
Type "Hello from vi inside vhs-rs"
Escape
Sleep 500ms

# Save and quit, then wait for the shell prompt to come back.
Type ":wq"
Enter
Wait

Type "cat notes.txt"
Enter
Wait

# Prove the editor session really wrote the file.
Assert /Hello from vi inside vhs-rs/

Sleep 1s
