# The website

<https://chainvue.github.io/pecu-cli/> — five pages, generated.

```sh
make serve     # build and serve on http://localhost:8000
make site      # build into web/_site only
```

## What is authored, and what is not

| Path | |
|---|---|
| `web/landing.html` | The landing page body. The only prose written for the site. |
| `web/assets/` | One stylesheet, one script, one favicon. No framework, no fetched fonts. |
| `web/build.py` | The generator. |
| `docs/*.md` | The four documentation pages, verbatim — the same files GitHub renders. |
| `docs/media/*-dark.svg` | The terminal captures, recorded by `docs/media/record.sh`. |

Nothing under `web/_site/` is authored. It is deleted and rewritten on every
build, and it is not committed.

## Decisions worth knowing before changing something

**One source for the reference.** The command reference is `docs/commands.md`
and nothing else. If a page here restated a flag, the two copies would disagree
within a month; the landing page is deliberately the only prose that lives in
`web/`, and it argues rather than documents.

**Links are relative.** No base URL is configured anywhere, so the output works
under `/pecu-cli/`, under `/`, and from a `file://` open, without a setting that
can be wrong in one of the three.

**The build fails rather than ships broken.** A link to a page or an anchor that
does not exist stops the build, as does a terminal capture whose class names
could not be namespaced. Both are silent in the browser and expensive to notice
later.

**The captures are inlined, not `<img>`.** svg-term names every class `a`..`o`
and every frame `1`..`n`, identically in every file it writes, so two captures
in one document would cross-wire. `build.py` prefixes each one. That is what
buys the replay button, the play-on-scroll, and the reduced-motion fallback —
none of which can reach inside an `<img>`.

**A capture rests on its last frame, not its first.** svg-term writes no `100%`
keyframe, so an animation asked to hold its end holds an empty window instead;
`build.py` restates the final offset at `100%`. Every failure mode — no
scripting, no `IntersectionObserver`, reduced motion — therefore lands on a
readable terminal.

**Dark captures on both themes.** A terminal is a dark surface, and a screenshot
of one reads better as a quotation than as something repainted to match the
page. It also sidesteps
[#47](https://github.com/chainvue/pecu-cli/issues/47): the light captures render
at 1.1:1 and are unreadable.

## Phone widths

```sh
make check-site    # no sideways scroll at 320/360/390/414, and Replay works
```

Worth its own command because the obvious way to check is wrong: headless Chrome
clamps its viewport to a 500px minimum, so `--window-size=390` renders a 500px
layout squeezed into a 390px image and everything looks fine. The script loads
each page in an iframe of the target width and reads `scrollWidth` from inside
it, which is the only honest measurement available here.

What it was written after: a `min-width` on the captures, added so a terminal
could be panned on a phone, made a 390px viewport scroll to 594px. Grid items do
not shrink below their content unless told to, so one wide child dragged every
paragraph's right edge off the screen — and the visible symptom was clipped
prose, nowhere near the capture that caused it.

The Replay half is there because the button shipped broken and survived three
rounds of review: every check asked whether it appeared and whether a handler was
attached, and none of them clicked it. `animation-name` lives in the capture's
own inline style and never changes, so toggling a class re-times the existing
animation rather than replacing it — and a finished animation that is re-timed is
still finished. The check sets the clock past the end, clicks, and reads it back.

**A capture rests on a different frame depending on the device.** Above the phone
breakpoint, with motion allowed and an `IntersectionObserver` available, it rests
on frame zero and plays when scrolled to. Otherwise it rests on its finished
frame. The choice is made in the inline `<head>` script, before first paint,
because `site.js` is deferred: deciding after the first frame is on screen is
what produces a flash rather than preventing one.

## Re-recording a capture

`docs/media/record.sh <name> <rows> <command…>` — read its header first. Both
warnings in it were written after the mistake they describe.
