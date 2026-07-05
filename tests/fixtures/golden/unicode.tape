# unicode.tape — non-ASCII content: double-width CJK cells, combining-free
# accented Latin, arrows, and box drawing. LANG/LC_ALL are pinned to C.UTF-8
# by vterm, so this renders identically everywhere.

Output unicode.txt

Set TypingSpeed 10ms
Set WaitTimeout 10s

Type "echo 'wide: 你好世界 | café naïve | → ✓'"
Enter
Wait

Type "printf '┌─┐\n│x│\n└─┘\n'"
Enter
Wait

Assert /你好世界/
