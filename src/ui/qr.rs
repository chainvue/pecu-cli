//! Carrying a payload across an air gap as QR codes.
//!
//! The gap is the point: a signing machine that never touches a network cannot
//! be reached by one either, and a camera or a screenshot is a one-way channel
//! narrow enough to reason about. A USB stick is the alternative, and it is a
//! two-way channel with an autorun history.
//!
//! # Framing
//!
//! A QR code holds at most 4296 alphanumeric characters, and a transaction with
//! several inputs outgrows that. So a payload is split into numbered frames:
//!
//! ```text
//! PECU1:2/5:A1B2C3…
//! ```
//!
//! Version, index, total, then the chunk. The header is what makes a stack of
//! codes reassemblable in any order and refusable when one is missing — a
//! payload silently short by one frame would be a transaction that fails at the
//! daemon for no visible reason.
//!
//! Hex is upper-cased, which is not cosmetic: QR's alphanumeric mode covers
//! `0-9 A-Z` and encodes at 5.5 bits per character, where byte mode spends 8.
//! The same payload in lower case needs roughly a third more modules.

use std::path::Path;

use miette::Diagnostic;
use qrcode::render::unicode::Dense1x2;
use qrcode::{EcLevel, QrCode};
use thiserror::Error;

/// Characters of payload per frame.
///
/// Well under the 4296 an alphanumeric QR can hold: a code near that ceiling is
/// version 40, which is 177×177 modules and needs a good camera and a large
/// screen. This keeps each frame around version 20 — comfortably scannable off
/// a terminal at an ordinary font size.
const CHUNK: usize = 1000;

const MAGIC: &str = "PECU1";

#[derive(Debug, Error, Diagnostic)]
pub enum QrError {
    #[error("cannot make a QR code from this")]
    #[diagnostic(code(pecu::qr_encode), help("{detail}"))]
    Encode { detail: String },

    #[error("cannot read {}", path.display())]
    #[diagnostic(code(pecu::qr_unreadable))]
    Unreadable {
        path: std::path::PathBuf,
        #[source]
        source: image::ImageError,
    },

    #[error("no QR code found in {}", path.display())]
    #[diagnostic(
        code(pecu::qr_not_found),
        help(
            "the whole code has to be in frame and in focus, with a little white space around it"
        )
    )]
    NotFound { path: std::path::PathBuf },

    #[error("frame {index} of {total} is missing")]
    #[diagnostic(
        code(pecu::qr_incomplete),
        help("every frame is needed — a payload short by one is a transaction that fails at the daemon for no visible reason")
    )]
    MissingFrame { index: usize, total: usize },

    #[error("these frames are not all from the same payload")]
    #[diagnostic(
        code(pecu::qr_mismatch),
        help("frames say {a} and {b} total; they came from different plans")
    )]
    Mismatched { a: usize, b: usize },

    #[error("that is a QR code, but not one of ours")]
    #[diagnostic(code(pecu::qr_foreign), help("expected a payload starting `{MAGIC}:`"))]
    Foreign,

    #[error("cannot write {}", path.display())]
    #[diagnostic(code(pecu::qr_unwritable))]
    Unwritable {
        path: std::path::PathBuf,
        #[source]
        source: image::ImageError,
    },
}

/// Split a hex payload into framed chunks, ready to encode.
pub fn frames(hex: &str) -> Vec<String> {
    let upper = hex.to_ascii_uppercase();
    let chars: Vec<char> = upper.chars().collect();
    let total = chars.len().div_ceil(CHUNK).max(1);
    chars
        .chunks(CHUNK)
        .enumerate()
        .map(|(index, chunk)| {
            let body: String = chunk.iter().collect();
            format!("{MAGIC}:{}/{total}:{body}", index + 1)
        })
        .collect()
}

/// Render one frame as block characters, two QR rows per line.
pub fn render(frame: &str) -> Result<String, QrError> {
    let code =
        QrCode::with_error_correction_level(frame.as_bytes(), EcLevel::M).map_err(|error| {
            QrError::Encode {
                detail: error.to_string(),
            }
        })?;
    Ok(code
        .render::<Dense1x2>()
        .quiet_zone(true)
        .dark_color(Dense1x2::Light)
        .light_color(Dense1x2::Dark)
        .build())
}

/// Write one frame as a PNG.
///
/// The image is drawn from the modules rather than through `qrcode`'s own image
/// renderer, which would tie this to whichever `image` version that crate
/// happens to depend on. Two crates' `Luma<u8>` are not the same type.
pub fn write_png(path: &Path, frame: &str) -> Result<(), QrError> {
    const SCALE: u32 = 8;
    /// Four modules of white on every side. Less and a decoder loses the
    /// finder patterns against whatever the code is printed on.
    const QUIET: u32 = 4;

    let code =
        QrCode::with_error_correction_level(frame.as_bytes(), EcLevel::M).map_err(|error| {
            QrError::Encode {
                detail: error.to_string(),
            }
        })?;
    let modules = code.to_colors();
    let width = code.width() as u32;
    let side = (width + QUIET * 2) * SCALE;

    let image = image::GrayImage::from_fn(side, side, |x, y| {
        let (mx, my) = (x / SCALE, y / SCALE);
        let inside = mx >= QUIET && my >= QUIET && mx < QUIET + width && my < QUIET + width;
        let dark = inside
            && modules[((my - QUIET) * width + (mx - QUIET)) as usize] == qrcode::Color::Dark;
        image::Luma([if dark { 0u8 } else { 255u8 }])
    });

    image.save(path).map_err(|source| QrError::Unwritable {
        path: path.to_path_buf(),
        source,
    })
}

/// Read every QR code out of a PNG.
pub fn read_png(path: &Path) -> Result<Vec<String>, QrError> {
    let image = image::open(path)
        .map_err(|source| QrError::Unreadable {
            path: path.to_path_buf(),
            source,
        })?
        .to_luma8();
    let mut prepared = rqrr::PreparedImage::prepare(image);
    let found: Vec<String> = prepared
        .detect_grids()
        .into_iter()
        .filter_map(|grid| grid.decode().ok().map(|(_, content)| content))
        .collect();
    if found.is_empty() {
        return Err(QrError::NotFound {
            path: path.to_path_buf(),
        });
    }
    Ok(found)
}

/// Reassemble framed chunks back into the payload.
///
/// Order does not matter and duplicates are fine — a stack of photographs is
/// rarely tidy. A *missing* frame is refused by number, because the alternative
/// is a truncated payload that looks like a corrupt transaction.
pub fn reassemble(frames: &[String]) -> Result<String, QrError> {
    let mut total: Option<usize> = None;
    let mut chunks: Vec<Option<String>> = Vec::new();

    for frame in frames {
        let rest = frame
            .strip_prefix(&format!("{MAGIC}:"))
            .ok_or(QrError::Foreign)?;
        let (counts, body) = rest.split_once(':').ok_or(QrError::Foreign)?;
        let (index, count) = counts.split_once('/').ok_or(QrError::Foreign)?;
        let index: usize = index.parse().map_err(|_| QrError::Foreign)?;
        let count: usize = count.parse().map_err(|_| QrError::Foreign)?;
        if index == 0 || index > count {
            return Err(QrError::Foreign);
        }

        match total {
            None => {
                total = Some(count);
                chunks = vec![None; count];
            }
            Some(seen) if seen != count => return Err(QrError::Mismatched { a: seen, b: count }),
            Some(_) => {}
        }
        chunks[index - 1] = Some(body.to_string());
    }

    let total = total.unwrap_or(0);
    let mut out = String::new();
    for (position, chunk) in chunks.iter().enumerate() {
        match chunk {
            Some(body) => out.push_str(body),
            None => {
                return Err(QrError::MissingFrame {
                    index: position + 1,
                    total,
                })
            }
        }
    }
    Ok(out.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(len: usize) -> String {
        std::iter::repeat_n("ab", len / 2).collect()
    }

    #[test]
    fn a_short_payload_is_one_frame_and_round_trips() {
        let hex = payload(100);
        let framed = frames(&hex);
        assert_eq!(framed.len(), 1);
        assert!(framed[0].starts_with("PECU1:1/1:"));
        assert_eq!(reassemble(&framed).unwrap(), hex);
    }

    #[test]
    fn a_long_payload_is_split_and_round_trips() {
        let hex = payload(3500);
        let framed = frames(&hex);
        assert_eq!(framed.len(), 4);
        assert_eq!(reassemble(&framed).unwrap(), hex);
    }

    #[test]
    fn frames_reassemble_out_of_order_because_photographs_are_untidy() {
        let hex = payload(3000);
        let mut framed = frames(&hex);
        framed.reverse();
        assert_eq!(reassemble(&framed).unwrap(), hex);
    }

    #[test]
    fn a_duplicate_frame_is_not_a_problem() {
        let hex = payload(2500);
        let mut framed = frames(&hex);
        framed.push(framed[0].clone());
        assert_eq!(reassemble(&framed).unwrap(), hex);
    }

    #[test]
    fn a_missing_frame_is_refused_by_number() {
        let hex = payload(3000);
        let framed = frames(&hex);
        let short: Vec<String> = framed.into_iter().skip(1).collect();
        let error = reassemble(&short).unwrap_err();
        let QrError::MissingFrame { index, .. } = error else {
            panic!("expected MissingFrame, got {error:?}");
        };
        assert_eq!(index, 1);
    }

    #[test]
    fn frames_from_two_different_payloads_are_refused() {
        let mut mixed = frames(&payload(2500));
        mixed.push(frames(&payload(600))[0].clone());
        assert!(matches!(
            reassemble(&mixed).unwrap_err(),
            QrError::Mismatched { .. }
        ));
    }

    #[test]
    fn somebody_elses_qr_code_is_refused() {
        assert!(matches!(
            reassemble(&["https://example.com".to_string()]).unwrap_err(),
            QrError::Foreign
        ));
    }

    #[test]
    fn a_frame_renders_to_block_characters() {
        let rendered = render(&frames(&payload(80))[0]).unwrap();
        assert!(rendered.lines().count() > 10, "suspiciously small QR");
        assert!(rendered.contains('█') || rendered.contains('▀') || rendered.contains('▄'));
    }

    #[test]
    fn a_frame_survives_a_png_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("frame.png");
        let hex = payload(400);
        let framed = frames(&hex);

        write_png(&path, &framed[0]).expect("written");
        let read = read_png(&path).expect("read back");
        assert_eq!(read, framed);
        assert_eq!(reassemble(&read).unwrap(), hex);
    }
}
