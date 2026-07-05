# theme-gallery.tape — render the same screen under several themes.
#
# `Set Theme` is one of the few settings allowed mid-tape (it does not change
# terminal geometry), so a single run can produce one screenshot per theme.
#
#   vterm run examples/theme-gallery.tape

Set TypingSpeed 20ms

Type "printf '\\e[31mred\\e[0m \\e[32mgreen\\e[0m \\e[34mblue\\e[0m \\e[1mbold\\e[0m\\n'"
Enter
Wait

Screenshot theme-default.png

Set Theme "Dracula"
Screenshot theme-dracula.png

Set Theme "Catppuccin Mocha"
Screenshot theme-catppuccin.png

Set Theme "GruvboxDark"
Screenshot theme-gruvbox.png
