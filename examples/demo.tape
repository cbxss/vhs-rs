# demo.tape — VHS-style demo: render a short shell session to a GIF,
# grabbing a PNG screenshot along the way.
#
#   vterm run examples/demo.tape

Output demo.gif

Set Width 1200
Set Height 600
Set FontSize 22
Set Padding 40
Set TypingSpeed 50ms

Type "echo 'Hello from vterm!'"
Enter
Wait

Sleep 500ms

Type "ls -la"
Enter
Wait

Screenshot demo.png

Sleep 1s
