# styling.tape — a commented tour of every visual knob vhs-rs offers.
#
#   vhs-rs run examples/styling.tape
#
# Produces styling.gif (the animated demo) and styling.png (the final frame).
# Geometry and chrome settings must appear before the first action command —
# the terminal grid is fixed once the shell spawns. Only Theme, TypingSpeed,
# PlaybackSpeed, WaitTimeout, and WaitPattern may change mid-tape.

Output styling.gif

# ---- Canvas -----------------------------------------------------------------
Set Width 720            # GIF width in pixels
Set Height 440           # GIF height in pixels
Set FontSize 15          # cell size derives from this; cols/rows follow
Set LineHeight 1.2       # vertical breathing room between rows (multiplier)
Set LetterSpacing 1      # extra horizontal pixels per cell

# ---- Chrome -----------------------------------------------------------------
# Any VHS theme name works (the full catalog is embedded), or pass a custom
# theme inline as JSON: Set Theme {"background": "#1e1e2e", ...}
Set Theme "Catppuccin Mocha"
Set WindowBar Colorful   # Colorful | ColorfulRight | Rings | RingsRight
Set WindowBarSize 32     # bar height in pixels
Set BorderRadius 10      # rounded corners on the terminal window
Set Padding 16           # space between the text and the window edge
Set Margin 18            # space between the window and the GIF edge
Set MarginFill "#89b4fa" # margin color; defaults to the theme background

# ---- Motion -----------------------------------------------------------------
Set TypingSpeed 45ms     # delay per keystroke — 30-60ms reads as human typing
Set PlaybackSpeed 1.0    # >1 speeds the final GIF up, <1 slows it down
Set Framerate 50         # frame cap; 50 is the GIF format's practical maximum
Set CursorBlink true     # synthesize cursor blinks during idle gaps
# Set LoopOffset 20%     # start the loop 20% in, so replays skip the cold
                         # open. Needs the exact-palette encoder (≤256 unique
                         # colors); this tape's antialiasing over a colored
                         # margin exceeds that, so it stays off here.

# ---- Hide the boring setup ---------------------------------------------------
# Nothing between Hide and Show is recorded: use it to cut clears, cd's, and
# environment prep out of the final GIF. Defining the noisy printf as a
# function here means the on-camera keystrokes stay clean — the viewer sees
# `colors`, not a wall of escape codes. (Tape strings have no escape
# sequences: `\e` reaches bash verbatim, which is exactly what printf wants.)
Hide
Type "colors() { printf '\e[1;35mbold magenta\e[0m  \e[32mgreen\e[0m  \e[4;34munderlined blue\e[0m\n'; }; clear"
Enter
Wait
Show

# ---- The demo — pace it for a reader, not a parser ---------------------------
Type "echo 'Pretty terminal GIFs — one binary, no browser'"
Sleep 500ms              # a beat before Enter lets the eye catch up
Enter
Wait

Type "colors"
Sleep 400ms
Enter
Wait
Sleep 1s                 # hold the finished screen before the loop restarts

# Screenshot grabs the current frame as a PNG (plus a .txt sibling with the
# same screen as plain text).
Screenshot styling.png
