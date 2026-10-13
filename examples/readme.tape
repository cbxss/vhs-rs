# Generated and verified with: npm run assets --prefix npm
# The runner supplies an empty scratch directory and captures session.jsonl.
Output readme.gif
Require vi
Set Width 700
Set Height 420
Set FontSize 17
Set Padding 18
Set Theme "Catppuccin Mocha"
Set WindowBar Colorful
Set BorderRadius 8
Set Margin 14
Set MarginFill "#7287fd"
Set TypingSpeed 60ms

Type "vi -u NONE -i NONE -n notes.txt"
Enter
Wait+Screen@10s /(?m)^"notes\.txt"/
Type "i"
Type "Script your terminal."
Enter
Type "Test what it shows."
Enter
Type "Record and replay."
Escape
Sleep 800ms
Type ":wq"
Enter
Wait

Type "clear; cat notes.txt"
Enter
Wait
Sleep 700ms
Type "printf 'Saved lines: '; wc -l < notes.txt"
Enter
Wait
Assert /(?m)^Saved lines: *3$/
Screenshot proof.png
Sleep 2s
