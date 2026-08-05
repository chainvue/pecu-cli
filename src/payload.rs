//! Getting bytes in and out of the air-gap commands.
//!
//! `plan`, `sign` and `broadcast` all pass hex between machines, and every one
//! of them can take it as an argument, from a file, or on stdin — because which
//! is convenient depends on how the gap is actually crossed. A USB stick wants
//! a file; a terminal on the other side of a KVM wants a paste.

use std::io::Read;
use std::path::PathBuf;

use miette::Diagnostic;
use thiserror::Error;

#[derive(Debug, Error, Diagnostic)]
pub enum PayloadError {
    #[error("nothing to read")]
    #[diagnostic(
        code(pecu::no_payload),
        help("give it as an argument, as @path/to/file, or `-` to read stdin")
    )]
    Empty,

    #[error("cannot read {}", path.display())]
    #[diagnostic(code(pecu::payload_unreadable))]
    Unreadable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("cannot read stdin")]
    #[diagnostic(code(pecu::stdin_unreadable))]
    Stdin {
        #[source]
        source: std::io::Error,
    },

    #[error("that is not hex")]
    #[diagnostic(code(pecu::not_hex), help("{detail}"))]
    NotHex { detail: String },

    #[error("cannot write {}", path.display())]
    #[diagnostic(code(pecu::payload_unwritable))]
    Unwritable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Read a hex payload from an argument, a `@file`, or stdin.
///
/// `None` and `-` both mean stdin, so a command reads a pipe by default rather
/// than sitting silently waiting for one.
pub fn read_hex(input: Option<&str>) -> Result<Vec<u8>, PayloadError> {
    let text = match input {
        None | Some("-") => {
            let mut buffer = String::new();
            std::io::stdin()
                .read_to_string(&mut buffer)
                .map_err(|source| PayloadError::Stdin { source })?;
            buffer
        }
        Some(argument) => match argument.strip_prefix('@') {
            Some(path) => {
                let path = PathBuf::from(path);
                std::fs::read_to_string(&path)
                    .map_err(|source| PayloadError::Unreadable { path, source })?
            }
            None => argument.to_string(),
        },
    };

    // Whitespace is how a payload survives being pasted across a terminal, an
    // email or a QR reader that wrapped it, so it is stripped rather than
    // treated as corruption.
    let cleaned: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if cleaned.is_empty() {
        return Err(PayloadError::Empty);
    }
    hex::decode(&cleaned).map_err(|error| PayloadError::NotHex {
        detail: error.to_string(),
    })
}

/// Read a payload that is *not* hex: whatever bytes were given, verbatim.
///
/// The counterpart to [`read_hex`], for VDXF values, where the payload is
/// arbitrary content rather than an encoding of something. Nothing is stripped
/// — a value's whitespace is part of it — except the single trailing newline a
/// shell or an editor adds, which is almost never meant and is easy to add back
/// deliberately with a file.
///
/// `None` and `-` both mean stdin.
pub fn read_bytes(input: Option<&str>) -> Result<Vec<u8>, PayloadError> {
    let bytes = match input {
        None | Some("-") => {
            let mut buffer = Vec::new();
            std::io::stdin()
                .read_to_end(&mut buffer)
                .map_err(|source| PayloadError::Stdin { source })?;
            buffer
        }
        Some(argument) => match argument.strip_prefix('@') {
            Some(path) => {
                let path = PathBuf::from(path);
                // Bytes, not a string: a value may legitimately not be text.
                std::fs::read(&path).map_err(|source| PayloadError::Unreadable { path, source })?
            }
            None => argument.as_bytes().to_vec(),
        },
    };

    Ok(match bytes.strip_suffix(b"\n") {
        Some(trimmed) => trimmed.strip_suffix(b"\r").unwrap_or(trimmed).to_vec(),
        None => bytes,
    })
}

/// Write hex to a file, with a trailing newline so `cat` behaves.
pub fn write_hex(path: &std::path::Path, hex: &str) -> Result<(), PayloadError> {
    std::fs::write(path, format!("{hex}\n")).map_err(|source| PayloadError::Unwritable {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_is_read_from_an_argument() {
        assert_eq!(read_hex(Some("00ff")).unwrap(), vec![0x00, 0xff]);
    }

    #[test]
    fn whitespace_from_a_paste_or_a_wrapped_qr_is_not_corruption() {
        assert_eq!(
            read_hex(Some("00 ff\n00\t ff")).unwrap(),
            vec![0x00, 0xff, 0x00, 0xff]
        );
    }

    #[test]
    fn a_file_is_read_with_an_at_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plan.hex");
        std::fs::write(&path, "00ff\n").unwrap();
        let argument = format!("@{}", path.display());
        assert_eq!(read_hex(Some(&argument)).unwrap(), vec![0x00, 0xff]);
    }

    #[test]
    fn a_missing_file_names_itself() {
        let error = read_hex(Some("@/nonexistent/plan.hex")).unwrap_err();
        assert!(
            matches!(error, PayloadError::Unreadable { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn rubbish_is_refused_as_rubbish() {
        assert!(matches!(
            read_hex(Some("zzzz")).unwrap_err(),
            PayloadError::NotHex { .. }
        ));
    }

    #[test]
    fn an_empty_argument_is_empty_not_an_empty_payload() {
        assert!(matches!(
            read_hex(Some("   ")).unwrap_err(),
            PayloadError::Empty
        ));
    }
}
