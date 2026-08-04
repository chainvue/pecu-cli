//! Snapshot the widget gallery.
//!
//! `pecu dev ui` renders every widget the kit has, so these two snapshots are a
//! diff on the whole look. Both run offline. The width is pinned with
//! `PECU_WIDTH` so the result does not depend on whoever's terminal ran them.

use assert_cmd::Command;

fn gallery(theme: &str) -> String {
    let output = Command::cargo_bin("pecu")
        .expect("the pecu binary should be built")
        .args(["dev", "ui", "--theme", theme])
        .env("PECU_WIDTH", "84")
        .env_remove("NO_COLOR")
        // The gallery renders a path row, and path rendering collapses $HOME to
        // `~`. Without a HOME there is nothing to collapse, so the snapshot does
        // not depend on whose machine ran it.
        .env_remove("HOME")
        .assert()
        .success();
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
