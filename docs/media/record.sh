#!/bin/sh
# Record one pecu command and render it as a light/dark pair of animated SVGs.
#
#   docs/media/record.sh <name> <rows> <command…>
#
# Two things this gets right that a hand-rolled recording gets wrong:
#
#   * It never pipes. `pecu` checks whether stdout is a terminal and renders
#     plain, unframed and uncoloured when it is not, so a demo piped through
#     `head` records the fallback rather than the tool. Size the window with
#     <rows> instead of trimming the output.
#
#   * It renders through `/tmp`. `svg-term` silently ignores `--profile` when
#     the profile path is inside the repository and falls back to its own
#     palette, which produces two "variants" that are byte-identical.
#
# Needs `asciinema` (v3) and `svg-term`. Neither is a build dependency; this is
# only run when the README's demos are regenerated.
set -e

name=$1; rows=$2; shift 2
[ -n "$name" ] && [ -n "$rows" ] && [ $# -gt 0 ] || {
    echo "usage: record.sh <name> <rows> <command…>" >&2; exit 2
}

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
work=$(mktemp -d)

# A keystore under $HOME, so the rendered path reads `~/.cache/...` rather than
# a temp directory nobody will recognise.
#
# This script MUST NOT delete it. An earlier version began with `rm -rf`, on the
# theory that a demo keystore is disposable — and wiped a key that had just been
# funded with 130 VRSCTEST to record a registration. The coins are still on
# chain and nobody can spend them. A directory holding private keys is not
# scratch space, however temporary its name sounds.
#
# If a demo needs a key that does not exist yet, create it before calling this,
# and remove a single stale key file by hand rather than the directory.
demo_home="$HOME/.cache/pecu-demo"
mkdir -p "$demo_home"

cat > "$work/play.sh" <<SCRIPT
#!/bin/sh
export PECU_WIDTH=74 PECU_PASSPHRASE=demo PECU_HOME="$demo_home"
sleep 0.6
printf '\033[38;5;120m\$\033[0m %s\n' "$*"
sleep 1.2
$*
sleep 3.5
SCRIPT
chmod +x "$work/play.sh"

cd "$repo"
PATH="$repo/target/release:$repo/target/debug:$PATH" \
    asciinema rec --overwrite -c "$work/play.sh" --window-size "78x$rows" "$work/rec.cast" >/dev/null 2>&1
asciinema convert --output-format asciicast-v2 "$work/rec.cast" "$work/rec-v2.cast" >/dev/null 2>&1

# Cap the dead air. A public node can take minutes to answer, and a two-phase
# registration waits for a block on purpose — making the reader sit through that
# is not more honest, only longer. Only the gaps shrink; every line still
# arrives in the order and at the relative pace it did.
python3 - "$work/rec-v2.cast" <<'CAP'
import json, sys
path = sys.argv[1]
lines = open(path).read().splitlines()
out, previous, shift = [], 0.0, 0.0
for line in lines[1:]:
    event = json.loads(line)
    gap = event[0] - previous
    previous = event[0]
    if gap > 2.0:
        shift += gap - 2.0
    event[0] = round(event[0] - shift, 3)
    out.append(json.dumps(event))
open(path, "w").write(lines[0] + "\n" + "\n".join(out) + "\n")
CAP

for variant in dark light; do
    cp "$here/$variant.xresources" "$work/$variant.xresources"
    svg-term --in "$work/rec-v2.cast" --out "$work/$name-$variant.svg" \
        --window --width 78 --height "$rows" \
        --term xresources --profile "$work/$variant.xresources" >/dev/null 2>&1
    cp "$work/$name-$variant.svg" "$here/$name-$variant.svg"
done

rm -rf "$work"
echo "$name: $(wc -c < "$here/$name-dark.svg") bytes"
