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

# A throwaway keystore under $HOME, so the rendered path reads `~/.cache/...`
# rather than a temp directory nobody will recognise.
demo_home="$HOME/.cache/pecu-demo"
rm -rf "$demo_home"
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

for variant in dark light; do
    cp "$here/$variant.xresources" "$work/$variant.xresources"
    svg-term --in "$work/rec-v2.cast" --out "$work/$name-$variant.svg" \
        --window --width 78 --height "$rows" \
        --term xresources --profile "$work/$variant.xresources" >/dev/null 2>&1
    cp "$work/$name-$variant.svg" "$here/$name-$variant.svg"
done

rm -rf "$work"
echo "$name: $(wc -c < "$here/$name-dark.svg") bytes"
