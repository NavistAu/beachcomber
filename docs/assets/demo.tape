# The README's terminal demo. Render it from the root of a beachcomber checkout, with `comb` and `hyperfine` on
# PATH (https://github.com/charmbracelet/vhs). The comparison reads the battery with `pmset`, so it is macOS-only.
#
#   vhs docs/assets/demo.tape
#
# It runs its own daemon on a private socket, so a daemon you already run is left alone. Every number in the
# recording is whatever this machine measures when you render it.

Output docs/assets/demo.gif

Set Shell zsh
Set FontSize 18
Set Width 1420
Set Height 460
Set Padding 24
Set TypingSpeed 45ms
Set CursorBlink false
# Every colour clears 7:1 against the background (WCAG AAA).
Set Theme { "name": "beachcomber", "background": "#0d1117", "foreground": "#e6edf3", "cursor": "#7fe9ff", "selection": "#264f78", "black": "#a0a9b4", "red": "#ffa198", "green": "#7ee787", "yellow": "#f2cc60", "blue": "#a5d6ff", "magenta": "#d2a8ff", "cyan": "#7fe9ff", "white": "#e6edf3", "brightBlack": "#a0a9b4", "brightRed": "#ffa198", "brightGreen": "#7ee787", "brightYellow": "#f2cc60", "brightBlue": "#a5d6ff", "brightMagenta": "#d2a8ff", "brightCyan": "#7fe9ff", "brightWhite": "#ffffff" }

Hide
# a single value prints without a trailing newline; zsh starts the next prompt on its own line regardless
Type "export BEACHCOMBER_SOCKET=${BEACHCOMBER_SOCKET:-/tmp/comb-demo-$UID.sock}; PROMPT='%F{cyan}beachcomber%f $ '; PROMPT_EOL_MARK=''; setopt interactive_comments" Enter
# warm the cache, so the recording shows reads rather than the first run
Type "comb g git . >/dev/null; clear" Enter
Sleep 1s
Show

# One key, then several: .s prints them as key='value' lines, ready for eval
Type "comb g git.branch ." Sleep 400ms Enter
Sleep 1.2s
Type "comb g.s git.branch git.dirty git.ahead git.behind git.upstream git.commit ." Sleep 400ms Enter
Sleep 2.5s

# A prompt segment, rendered from cached values in one round trip
Type "comb e '{{ git.branch }}{% if git.dirty %}*{% endif %} · {{ battery.percent }}%' ." Sleep 400ms Enter
Sleep 2s
Type "clear" Enter

# What that segment costs per render: running git and pmset, against reading comb's cache
Type "# both sides start a process; the gap is the work comb saves" Sleep 300ms Enter
Type `hyperfine -N -w 20 --style nocolor "sh -c 'git status -sb; pmset -g batt'" "comb e '{{ git.branch }} {{ battery.percent }}%' ."` Sleep 400ms Enter
# hyperfine takes as long as it takes; carry on once the prompt returns
Wait@120s /beachcomber \$\s*$/
Sleep 1s
Type "# an SDK client on a persistent connection skips the process start too" Sleep 300ms Enter
Sleep 4s

Hide
Type "comb k >/dev/null 2>&1" Enter
