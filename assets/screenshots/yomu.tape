Require target/release/yomu

Set Shell bash
Set FontFamily "Menlo"
Set FontSize 18
Set Width 1200
Set Height 800
Set Theme "Catppuccin Mocha"
Set Padding 24
Set TypingSpeed 25ms
Set CursorBlink false

Type "env -u NO_COLOR TERM=xterm-256color COLORTERM=truecolor ./target/release/yomu --graphics text --theme builtin:dark assets/screenshots/demo.md"
Enter
Wait+Screen /q quit  graphics text/
Sleep 1s
Screenshot "assets/screenshots/yomu.png"
Type "q"
