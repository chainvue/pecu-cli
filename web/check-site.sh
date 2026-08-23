#!/bin/sh
# Assert the two things about the built site that reading it cannot tell you:
# that no page scrolls sideways at phone widths, and that the Replay button on a
# terminal capture actually restarts the recording.
#
#   web/check-site.sh [width...]          (default: 320 360 390 414)
#
# Why this exists as a script rather than a habit: headless Chrome clamps its
# own viewport to a 500px minimum, so `--window-size=390` renders a 500px layout
# squeezed into a 390px image and everything looks fine. The only way to measure
# a real phone width is to load the page in an iframe of that width and read
# `scrollWidth` from inside it — which is what this does.
#
# The bug it was written after: a `min-width` on the terminal captures made a
# 390px viewport scroll to 594px. Grid items do not shrink below their content
# unless told to, so one wide child dragged every paragraph's right edge off the
# screen. Nothing in the Rust gate or the site build could see it.
#
# Needs Chrome and a built site. Not in CI: it is a browser dependency for a
# check that catches a class of bug rather than a regression in one place.
set -e
here=$(cd "$(dirname "$0")" && pwd)
site="$here/_site"
[ -f "$site/index.html" ] || { echo "no built site — run 'make site' first" >&2; exit 1; }

chrome=${CHROME:-/Applications/Google Chrome.app/Contents/MacOS/Google Chrome}
[ -x "$chrome" ] || { echo "no Chrome at $chrome — set CHROME=" >&2; exit 1; }

port=${PORT:-8402}
python3 -m http.server "$port" --bind 127.0.0.1 --directory "$site" >/dev/null 2>&1 &
server=$!
trap 'kill $server 2>/dev/null' EXIT
sleep 1

widths=${*:-"320 360 390 414"}
pages="/ /commands/ /configuration/ /design/ /status/ /404.html"
failed=0

for width in $widths; do
    for page in $pages; do
        cat > "$site/.probe.html" <<HTML
<!doctype html><meta charset=utf-8>
<style>html,body{margin:0}iframe{width:${width}px;height:900px;border:0}</style>
<iframe id=f src="$page"></iframe>
<script>document.getElementById('f').onload=function(){setTimeout(function(){
var d=this.contentDocument;
document.title=d.documentElement.clientWidth+' '+d.documentElement.scrollWidth;
}.bind(this),900);};</script>
HTML
        got=$("$chrome" --headless=new --disable-gpu --window-size=$((width + 60)),950 \
              --virtual-time-budget=7000 --dump-dom "http://127.0.0.1:$port/.probe.html" 2>/dev/null \
              | sed -n 's/.*<title>\([0-9]* [0-9]*\)<\/title>.*/\1/p')
        viewport=${got% *}
        scroll=${got#* }
        if [ "$viewport" != "$width" ]; then
            echo "  ?? ${width}px ${page}: iframe reported ${viewport}px" >&2
            failed=1
        elif [ "$scroll" -gt "$viewport" ]; then
            echo "  OVERFLOW ${width}px ${page}: scrollWidth ${scroll}" >&2
            failed=1
        else
            echo "  ok ${width}px ${page}"
        fi
    done
done

# --- the Replay button ------------------------------------------------------
#
# It was shipped broken and stayed broken through three rounds of review,
# because every check asked whether the button appeared and whether a handler
# was attached. Nobody clicked it. `animation-name` lives in the capture's own
# inline style and never changes, so toggling a class re-times the existing
# animation instead of replacing it — and a finished animation that is re-timed
# is still finished. This drives the click and reads the clock.
cat > "$site/.replay.html" <<'HTML'
<!doctype html><meta charset=utf-8>
<style>html,body{margin:0}iframe{width:1200px;height:900px;border:0}</style>
<iframe id=f src="/"></iframe>
<script>
document.getElementById('f').onload = function () {
  var d = this.contentDocument, bad = [];
  setTimeout(function () {
    d.querySelectorAll('[data-term]').forEach(function (term, i) {
      var film = term.querySelector('svg g[style*="animation-name"]');
      if (!film) { bad.push(i + ':no-film'); return; }
      var button = term.querySelector('[data-replay]');
      if (!button || button.hidden) { bad.push(i + ':no-button'); return; }
      term.classList.add('is-playing');
      var before = film.getAnimations()[0];
      if (!before) { bad.push(i + ':no-animation'); return; }
      before.currentTime = 9000;                 // past every recorded duration
      button.click();
      var after = film.getAnimations()[0];
      if (after.playState !== 'running' || after.currentTime > 100) {
        bad.push(i + ':' + Math.round(after.currentTime) + '/' + after.playState);
      }
    });
    document.title = bad.length ? 'DEAD ' + bad.join(',') : 'OK';
  }, 900);
};
</script>
HTML
verdict=$("$chrome" --headless=new --disable-gpu --window-size=1240,950 \
          --virtual-time-budget=8000 --dump-dom "http://127.0.0.1:$port/.replay.html" 2>/dev/null \
          | sed -n 's/.*<title>\(.*\)<\/title>.*/\1/p')
rm -f "$site/.replay.html"

case "$verdict" in
    OK) echo "  ok replay restarts every capture" ;;
    *)  echo "  REPLAY does not restart: $verdict" >&2; failed=1 ;;
esac

rm -f "$site/.probe.html"
[ "$failed" -eq 0 ] || { echo "the built site has a defect a reader would hit" >&2; exit 1; }
echo "no page scrolls sideways, and Replay restarts every capture"
