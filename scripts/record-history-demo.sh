#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
QUERY="${1:-nix}"
OUT="${2:-$ROOT/demo/fff-history-demo-kitty.mp4}"
FRAME="${OUT%.mp4}-frame.png"
TITLE="fff-history-demo-$$"
KITTY_APP="$HOME/.nix-profile/Applications/kitty.app"
KITTY_BIN="$KITTY_APP/Contents/MacOS/kitty"
KITTY_RC_BIN="${KITTY_RC_BIN:-kitten}"
SOCK="/tmp/${TITLE}.sock"
SUFFIX=" build"
TMP_DEMO_DIR="$(mktemp -d /tmp/fff-history-demo.XXXXXX)"
FFF_BIN="$ROOT/target/release/fff"
PROMPT_TEXT="~/m/d/n/fff-tui % "
START_FRAME=6
FRAME_COUNT=24
MP4_WIDTH=1080
GIF_WIDTH=720
STILL_AT=1.7
MAX_CAPTURE_FRAMES=40

send_chars() {
  local text="$1"
  local delay="${2:-0.2}"
  local i char
  for ((i = 0; i < ${#text}; i++)); do
    char="${text:i:1}"
    "$KITTY_RC_BIN" @ --to "unix:$SOCK" send-text "$char" >/dev/null
    sleep "$delay"
  done
}

capture_window_frame() {
  local window_id="$1"
  local out="$2"
  local attempts="${3:-10}"
  local i
  for ((i = 0; i < attempts; i++)); do
    if screencapture -x -l "$window_id" "$out" >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.1
  done
  return 1
}

window_id_for_title() {
  local title="$1"
  swift -e '
import CoreGraphics
import Foundation

let target = CommandLine.arguments[1]
let wins = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as? [[String: Any]] ?? []
for w in wins {
    if (w[kCGWindowOwnerName as String] as? String) == "kitty",
       (w[kCGWindowName as String] as? String) == target,
       let n = w[kCGWindowNumber as String] as? Int {
        print(n)
        exit(0)
    }
}
exit(1)
' "$title"
}

wait_for_window_id() {
  local title="$1"
  local attempts="${2:-40}"
  local i
  for ((i = 0; i < attempts; i++)); do
    if window_id_for_title "$title"; then
      return 0
    fi
    sleep 0.2
  done
  return 1
}

cleanup_demo_window() {
  osascript <<APPLESCRIPT >/dev/null 2>&1 || true
tell application "System Events"
  if exists process "kitty" then
    tell process "kitty"
      repeat with w in (every window)
        try
          if value of attribute "AXTitle" of w is "$TITLE" then
            set frontmost to true
            perform action "AXRaise" of w
            keystroke "w" using command down
            delay 0.2
          end if
        end try
      end repeat
    end tell
  end if
end tell
APPLESCRIPT
  rm -rf "$TMP_DEMO_DIR"
}

trap cleanup_demo_window EXIT

cat >"$TMP_DEMO_DIR/history-demo.sh" <<EOF
#!/bin/sh
cd "$ROOT"
printf '\\033[36m%s\\033[0m' "$PROMPT_TEXT"
selected="\$(HISTFILE="$HOME/.zsh_history" "$FFF_BIN" history)"
status=\$?
printf '\\r\\033[2K'
printf '\\033[36m%s\\033[0m' "$PROMPT_TEXT"
if [ \$status -eq 0 ] && [ -n "\$selected" ]; then
  printf '%s' "\$selected"
fi
sleep 2
EOF
chmod +x "$TMP_DEMO_DIR/history-demo.sh"

osascript <<APPLESCRIPT >/dev/null
do shell script "rm -f " & quoted form of "$SOCK"
do shell script quoted form of POSIX path of (POSIX file "$KITTY_BIN") & " --single-instance=no -o allow_remote_control=socket-only --listen-on=unix:$SOCK -o update_window_title=no -o background_opacity=1.0 -o dynamic_background_opacity=no -o background_blur=0 -o font_size=24 -o initial_window_width=84c -o initial_window_height=11c -T $TITLE sh -lc 'exec $TMP_DEMO_DIR/history-demo.sh' >/dev/null 2>&1 &"
delay 1
tell application "System Events"
  if exists process "kitty" then
    tell process "kitty"
      set frontmost to true
    end tell
  end if
end tell
APPLESCRIPT

window_id="$(wait_for_window_id "$TITLE")" || {
  echo "failed to resolve kitty window id" >&2
  exit 1
}

mkdir -p "$(dirname "$OUT")"
rm -f "$OUT" "$FRAME"
tmp_frames="$(mktemp -d /tmp/fff-history-demo-frames.XXXXXX)"

sleep 0.8

(
  for ((i = 0; i < MAX_CAPTURE_FRAMES; i++)); do
    capture_window_frame "$window_id" "$tmp_frames/frame_$(printf '%04d' "$i").png" || break
    sleep 0.1
  done
) &
cap_pid=$!

sleep 1.2
osascript <<APPLESCRIPT >/dev/null 2>&1 || true
tell application "System Events"
  if exists process "kitty" then
    tell process "kitty"
      set frontmost to true
    end tell
  end if
end tell
APPLESCRIPT
send_chars "$QUERY" 0.22
sleep 0.8
send_chars "$SUFFIX" 0.18
sleep 1.2
"$KITTY_RC_BIN" @ --to "unix:$SOCK" send-key enter >/dev/null
sleep 1.4

wait "$cap_pid"
ffmpeg -y -framerate 10 -start_number "$START_FRAME" -i "$tmp_frames/frame_%04d.png" -frames:v "$FRAME_COUNT" -vf "scale=$MP4_WIDTH:-2:flags=lanczos,format=yuv420p" "$OUT" >/tmp/fff-history-demo-kitty.log 2>&1
ffmpeg -y -i "$OUT" -vf "fps=10,scale=$GIF_WIDTH:-1:flags=lanczos" "$TMP_DEMO_DIR/gif-frame-%03d.png" >/tmp/fff-history-demo-gif-frames.log 2>&1
magick -dispose previous -delay 10 -loop 0 "$TMP_DEMO_DIR"/gif-frame-*.png "${OUT%.mp4}.gif"
ffmpeg -y -ss "$STILL_AT" -i "$OUT" -frames:v 1 -update 1 "$FRAME" >/tmp/fff-history-demo-kitty-frame.log 2>&1
rm -rf "$tmp_frames"

echo "$OUT"
