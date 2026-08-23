#!/bin/sh
# Render web/og.html to web/assets/og.png at exactly 1200x630.
#
# Run this by hand and commit the PNG. It is deliberately NOT part of the build:
# link-preview cards change about once a year, and making every deploy depend on
# a headless browser to redraw an unchanged image is a bad trade.
#
# Needs Chrome. On Linux, swap in `google-chrome` or `chromium`.
set -e
here=$(cd "$(dirname "$0")" && pwd)
chrome=${CHROME:-/Applications/Google Chrome.app/Contents/MacOS/Google Chrome}
[ -x "$chrome" ] || { echo "no Chrome at $chrome — set CHROME=" >&2; exit 1; }

"$chrome" --headless=new --disable-gpu --hide-scrollbars \
  --window-size=1200,630 --default-background-color=0d1117ff \
  --screenshot="$here/assets/og.png" "file://$here/og.html" >/dev/null 2>&1

echo "assets/og.png: $(wc -c < "$here/assets/og.png") bytes"
