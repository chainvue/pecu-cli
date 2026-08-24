//! Snapshot the widget gallery.
//!
//! `pecu dev ui` renders every widget the kit has, so these two snapshots are a
//! diff on the whole look. Both run offline. The width is pinned with
//! `PECU_WIDTH` so the result does not depend on whoever's terminal ran them.

use assert_cmd::Command;

fn gallery(theme: &str) -> String {
    render(theme, false)
}

/// The same gallery with the colour left in. `anstream` strips escapes when the
/// stream is not a tty, and a test harness never is, so seeing what a skin
/// actually paints takes `CLICOLOR_FORCE`.
fn gallery_in_colour(theme: &str) -> String {
    render(theme, true)
}

fn render(theme: &str, colour: bool) -> String {
    let mut command = Command::cargo_bin("pecu").expect("the pecu binary should be built");
    command
        .args(["dev", "ui", "--theme", theme])
        .env("PECU_WIDTH", "84")
        .env_remove("NO_COLOR")
        // `--theme` reads it, so a developer who exported one would otherwise
        // be running a different test from CI.
        .env_remove("PECU_THEME")
        // The gallery renders a path row, and path rendering collapses $HOME to
        // `~`. Without a HOME there is nothing to collapse, so the snapshot does
        // not depend on whose machine ran it.
        .env_remove("HOME");
    if colour {
        command.env("CLICOLOR_FORCE", "1");
    } else {
        command.env_remove("CLICOLOR_FORCE");
    }
    let output = command.assert().success();
    String::from_utf8(output.get_output().stdout.clone()).expect("output should be utf-8")
}

#[test]
fn phosphor_gallery() {
    // Colour is stripped by anstream because the test harness is not a tty, so
    // what this pins is the geometry: frames, alignment, glyphs.
    insta::assert_snapshot!("phosphor", gallery("phosphor"));
}

#[test]
fn plain_gallery() {
    insta::assert_snapshot!("plain", gallery("plain"));
}

#[test]
fn the_plain_gallery_contains_no_escapes_or_box_drawing() {
    let rendered = gallery("plain");
    assert!(
        !rendered.contains('\u{1b}'),
        "ANSI escapes leaked into --theme plain"
    );
    for glyph in ['│', '┌', '└', '├', '─', '…', '▸'] {
        assert!(
            !rendered.contains(glyph),
            "`{glyph}` leaked into --theme plain"
        );
    }
}

#[test]
fn auto_falls_back_to_plain_when_output_is_not_a_terminal() {
    assert_eq!(gallery("auto"), gallery("plain"));
}

/// The light skin is the phosphor skin re-inked, not a second layout: same
/// frames, same alignment, same glyphs. With the colour stripped the two are
/// byte-identical — which is exactly what lets one recording be re-inked into
/// the other (`docs/media/relight.py`) instead of re-recorded.
#[test]
fn light_and_phosphor_differ_only_in_colour() {
    assert_eq!(gallery("light"), gallery("phosphor"));
}

/// And `--theme light` reaches the palette rather than being accepted and
/// ignored: 235 is the light skin's value ink, 194 is phosphor's.
#[test]
fn the_light_skin_paints_its_own_colours() {
    let light = gallery_in_colour("light");
    assert!(light.contains("38;5;235"), "no light ink in --theme light");
    assert!(
        !light.contains("38;5;194"),
        "phosphor's value colour leaked into --theme light"
    );
    assert!(gallery_in_colour("phosphor").contains("38;5;194"));
}
