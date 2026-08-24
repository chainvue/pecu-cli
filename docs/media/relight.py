#!/usr/bin/env python3
"""Re-ink a light capture from the phosphor palette to the light one.

    docs/media/relight.py docs/media/*-light.svg

`record.sh` renders both variants from one recording, and `light.xresources`
only reaches the sixteen ANSI slots. Every colour `pecu` asks for is a
256-colour index above 15, so the light render came out with the phosphor
palette on a white background — `value` at #d7ffd7 is 1.10:1 there, invisible,
while the labels beside it stayed legible. That is what this fixes.

It is a substitution, not a recolouring by taste: each phosphor index is
replaced by the colour `Palette::light()` gives the same role, so a relit
capture is a faithful rendering of `pecu --theme light`. Keep this table and
`Palette::light()` in `src/ui/theme.rs` in step — the unit test
`the_published_light_captures_are_the_light_palette` fails if they drift.

One colour here is not a palette role: index 120 is the `$` prompt this script's
neighbour prints around the demo command. It takes the light theme's green.
"""

import pathlib
import re
import sys

# phosphor -> light, keyed on the colour rather than on the CSS class: svg-term
# emits a cascade of grouped overrides and the class letters are not stable
# between files, so `.k` is the value column in one capture and the accent in
# the next. Applied in one pass, over the original text, so a target colour is
# never itself re-substituted.
#
# The table is not injective: indices 46 and 120 both land on #005f00, so the
# `$` sigil stops being a different green from a panel title. That is the one
# distinction the light capture loses, and it is deliberate — see the last
# paragraph of the docstring above.
INK = {
    "#008700": "#008700",  # frame   28 -> 28, already 4.70:1 on white
    "#00ff00": "#005f00",  # title/ok 46 -> 22
    "#8a8a8a": "#444444",  # label  245 -> 238
    "#d7ffd7": "#262626",  # value  194 -> 235
    "#5fff87": "#005faf",  # accent  84 -> 25
    "#585858": "#6c6c6c",  # muted  240 -> 242
    "#ffaf00": "#af5f00",  # warn   214 -> 130
    "#ff5f5f": "#af0000",  # danger 203 -> 124
    "#87ff87": "#005f00",  # the `$` prompt, 120 -> 22 (not a Palette role)
}

# What a relit capture is allowed to contain: the eight light inks above, plus
# the four colours that are not `pecu`'s to choose — the xresources background
# and foreground, and svg-term's own title-bar buttons and cursor.
#
# This is the guard that matters. Substituting the known table cannot fail, but
# it also cannot see an index that is not in it: add a tenth role to
# `Palette::phosphor`, or re-record with a `pecu` that paints something new, and
# the old table would relight the colours it recognises and leave the new one at
# phosphor brightness on white — the same invisible-value bug, in one column
# instead of the whole panel. So the check is on the output, not on the table.
CHROME = {"#ffffff", "#1f2328", "#ff5f58", "#ffbd2e", "#18c132", "#0969da", "#58a6ff"}

COLOUR = re.compile(r"#[0-9a-fA-F]{6}|#[0-9a-fA-F]{3}\b")


def full(colour):
    """`#FFF` and `#ffffff` are the same colour; compare them as one."""
    digits = colour.lstrip("#").lower()
    if len(digits) == 3:
        digits = "".join(c * 2 for c in digits)
    return f"#{digits}"


def relight(text):
    def swap(match):
        return INK.get(full(match.group(0)), match.group(0))

    return COLOUR.sub(swap, text)


def main(argv):
    paths = [pathlib.Path(a) for a in argv]
    if not paths:
        print(__doc__.splitlines()[2].strip(), file=sys.stderr)
        return 2
    for path in paths:
        before = path.read_text(encoding="utf-8")
        after = relight(before)
        known = set(INK.values()) | CHROME
        unknown = sorted({full(c) for c in COLOUR.findall(after)} - known)
        if unknown:
            print(
                f"{path}: {', '.join(unknown)} is not a colour this table knows, so it "
                f"was left at phosphor brightness on white; add it to INK",
                file=sys.stderr,
            )
            return 1
        path.write_text(after, encoding="utf-8")
        print(f"{path.name}: {'re-inked' if after != before else 'already light'}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
