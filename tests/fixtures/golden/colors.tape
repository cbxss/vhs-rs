# colors.tape — ANSI colors from printf plus `ls --color=always` over a
# tape-created temp directory, so the output never depends on the host's
# filesystem. Also takes a screenshot for the structural PNG checks.

Output colors.txt

Set Width 800
Set Height 400
Set TypingSpeed 10ms
Set WaitTimeout 10s

# vhs-rs pins TERM/LANG/LC_ALL/PS1 but inherits the rest of the environment;
# pin LS_COLORS so `ls --color` paints identically on every machine.
Env LS_COLORS "di=01;34:ex=01;32"

Type `cd "$(mktemp -d)" && mkdir sub && touch plain.txt run.sh && chmod +x run.sh`
Enter
Wait

Type "ls --color=always"
Enter
Wait

Type "printf '\e[31mred\e[0m \e[1;32mbold-green\e[0m \e[44mblue-bg\e[0m\n'"
Enter
Wait

Screenshot colors-shot.png
