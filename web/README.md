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

## Re-recording a capture

`docs/media/record.sh <name> <rows> <command…>` — read its header first. Both
warnings in it were written after the mistake they describe.
