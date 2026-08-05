//! `pecu id login` — proving you hold a VerusID without handing over a key.
//!
//! Three commands because there are three parties, and separating them is the
//! point: the site issues a challenge, the holder signs it somewhere else, the
//! site checks the answer. Nothing but the signature crosses between them, and
//! the signature is useless to anyone who did not ask for it — the audience is
//! inside the signed bytes.
//!
//! # Why `verify` keeps a file
//!
//! A signature over a fixed string is a bearer token. Anyone who observes one
//! can present it again, and the cryptography cannot tell the difference — the
//! bytes really are a valid signature by the right key over the right message.
//! The only defence is that the verifier remembers what it asked for and
//! accepts each challenge once.
//!
//! So `challenge` writes to `<config>/logins/<challenge>.json` and `verify`
//! *consumes* that file. A second attempt with the same challenge is refused
//! before the node is called. `--stateless` opts out, for a challenge that was
//! issued somewhere else and is tracked there; it says so in the output,
//! because a verification that is not checking replay should not look like one
//! that is.
//!
//! # Which identity, and when
//!
//! The SDK verifies against the identity **as it stood at the height the
//! signature carries**, not as it stands now — a key rotated out last week must
//! not invalidate a login made before the rotation. Revocation is the exception
//! and is retroactive: it is the break-glass action, and it takes effect
//! immediately. Both rules live in `verify_login`; this module supplies the
//! freshness bound, without which a signature never expires.

use std::path::PathBuf;

use miette::Diagnostic;
use thiserror::Error;
use verus_sdk::network::{
    sign_login, verify_login, FlowError, LoggedIn, LoginPolicy, LoginRequest,
};
use verus_sdk::signature::IdentitySignature;

use crate::cli::{LoginChallengeArgs, LoginSignArgs, LoginVerifyArgs};
use crate::config::Settings;
use crate::keystore::{self, Keystore};
use crate::node;
use crate::ui::{fmt, Panel, Text, Ui};

/// How much of a node-supplied identity name is ever printed.
const NAME_BUDGET: usize = 40;

/// How much of an audience string is ever printed. It is chosen by whoever
/// issued the challenge, which on the verifying side is us, and on the signing
/// side is not.
const AUDIENCE_BUDGET: usize = 60;

#[derive(Debug, Error, Diagnostic)]
pub enum LoginError {
    #[error("no key to sign with")]
    #[diagnostic(
        code(pecu::no_key),
        help("pass --from <label>, or make a key with `pecu key gen`")
    )]
    NoKey,

    #[error("the keystore holds {count} keys, so there is no obvious one to sign with")]
    #[diagnostic(
        code(pecu::ambiguous_key),
        help("name one with --from <label>; `pecu key list` shows them")
    )]
    AmbiguousKey { count: usize },

    #[error("that is not a signature this can read")]
    #[diagnostic(
        code(pecu::bad_signature),
        help("it should be the base64 string `pecu id login sign` printed — the same form `verusd signmessage` returns")
    )]
    BadSignature {
        #[source]
        source: verus_sdk::verus_tx::TxError,
    },

    #[error("this machine did not issue that challenge")]
    #[diagnostic(
        code(pecu::unknown_challenge),
        help("a signature is replayable by anyone who sees it, so only a challenge issued here and not yet used is accepted. Issue one with `pecu id login challenge`, or pass --stateless if it came from somewhere else and replay is tracked there")
    )]
    UnknownChallenge,

    #[error("that challenge was issued for `{issued}`, not `{given}`")]
    #[diagnostic(
        code(pecu::wrong_audience),
        help("the audience is part of the signed message; checking a signature against a different one would accept a login meant for somebody else")
    )]
    WrongAudience { issued: String, given: String },

    #[error("a challenge must be text a filename can hold")]
    #[diagnostic(
        code(pecu::bad_challenge),
        help("`pecu id login challenge` issues hex. This one has characters that cannot safely name a file")
    )]
    BadChallenge,

    #[error("the login store at {path} is not readable")]
    #[diagnostic(code(pecu::login_store), help("{advice}"))]
    Store {
        path: String,
        advice: String,
        #[source]
        source: std::io::Error,
    },

    #[error("{what} failed")]
    #[diagnostic(code(pecu::flow_failed), help("{advice}"))]
    Flow {
        what: &'static str,
        advice: String,
        #[source]
        source: Box<FlowError>,
    },
}

fn flow(what: &'static str, source: FlowError) -> LoginError {
    let advice = match &source {
        // Not a broken signature and not a broken node: the identity says no.
        // Saying "try --node" here would send someone chasing the wrong thing.
        FlowError::NotReady(_) => {
            "the chain answered — this is the answer, not a failure to get one".to_string()
        }
        FlowError::NoSuchIdentity(_) => {
            "check the name; `pecu id show <name@>` reads it off the chain".to_string()
        }
        _ => "run `pecu doctor`, or point somewhere else with --node".to_string(),
    };
    LoginError::Flow {
        what,
        advice,
        source: Box::new(source),
    }
}

/// A challenge this machine issued and has not yet accepted.
#[derive(serde::Serialize, serde::Deserialize)]
struct Issued {
    audience: String,
    /// Unix seconds, for display only. Freshness is decided by the block height
    /// inside the signature, which is the thing an attacker cannot backdate.
    issued_at: u64,
}

/// `pecu id login challenge` — ask someone to prove who they are.
pub fn challenge(ui: &Ui, settings: &Settings, args: &LoginChallengeArgs) -> miette::Result<()> {
    // 32 bytes from the OS. The SDK is explicit that a predictable or reused
    // challenge produces a credential that works forever, for whoever copies
    // it, so this is not a place to be clever with counters or timestamps.
    let bytes = keystore::entropy()?;
    let challenge = hex::encode(bytes.as_ref());

    let request = LoginRequest {
        audience: args.audience.clone(),
        challenge: challenge.clone(),
    };

    let path = record_path(settings, &challenge)?;
    save_issued(
        &path,
        &Issued {
            audience: args.audience.clone(),
            issued_at: now(),
        },
    )?;

    if ui.is_json() {
        emit(&serde_json::json!({
            "audience": args.audience,
            "challenge": challenge,
            "message": request.message_text(),
        }));
        return Ok(());
    }

    let palette = ui.theme.palette;
    ui.panel(
        &Panel::new("LOGIN CHALLENGE")
            .row("audience", Text::of(&args.audience, palette.value))
            .row("challenge", Text::of(&challenge, palette.accent))
            .section("MESSAGE TO SIGN")
            .wrapped(0, Text::of(request.message_text(), palette.muted))
            .note(Text::of(
                "single use. `pecu id login verify` accepts this challenge once and then \
                 forgets it, because a signature is replayable by anyone who sees it",
                palette.muted,
            ))
            .note(Text::of(
                format!(
                    "the signer runs: pecu id login sign <name@> --audience {:?} --challenge {challenge}",
                    args.audience
                ),
                palette.muted,
            )),
    );
    Ok(())
}

/// `pecu id login sign` — answer a challenge.
pub fn sign(ui: &Ui, settings: &Settings, args: &LoginSignArgs) -> miette::Result<()> {
    let store = Keystore::new(&settings.paths);
    let envelope = choose_key(&store, args.from.as_deref())?;
    let secret = keystore::passphrase(&format!("passphrase for `{}`", envelope.label), false)?;
    let key = envelope.unlock(&secret)?;

    let node = node::connect(&settings.profile)?;
    let request = LoginRequest {
        audience: args.audience.clone(),
        challenge: args.challenge.clone(),
    };

    ui.sdk(format!(
        "verus_sdk::network::sign_login(&node, &key, {:?}, &request)",
        args.name
    ));
    let signature = sign_login(&node, &key, &args.name, &request)
        .map_err(|source| flow("signing the challenge", source))?;
    ui.sdk_result(format!(
        "IdentitySignature {{ block_height: {}, signatures: {} }}",
        signature.block_height,
        signature.signatures.len()
    ));

    let encoded = signature.to_base64();

    if ui.is_json() {
        emit(&serde_json::json!({
            "identity": args.name,
            "audience": args.audience,
            "challenge": args.challenge,
            "signed_at": signature.block_height,
            "signed_by": envelope.address,
            "signature": encoded,
        }));
        return Ok(());
    }

    let palette = ui.theme.palette;
    ui.panel(
        &Panel::new("SIGNED")
            .row("identity", Text::of(&args.name, palette.accent))
            .row("signed by", Text::of(&envelope.address, palette.value))
            .row(
                "height",
                Text::of(fmt::height(signature.block_height.into()), palette.value),
            )
            .section("SIGNATURE")
            .wrapped(0, Text::of(&encoded, palette.value))
            .note(Text::of(
                "the height is stamped into the signature: a verifier checks it for freshness, \
                 and resolves the identity as it stood then rather than as it stands now",
                palette.muted,
            ))
            .note(Text::of(
                "this proves control at that height and nothing else. It is not a session, and \
                 anyone who copies it can present it — which is why the challenge is single use",
                palette.muted,
            )),
    );
    ui.explain_panel();
    Ok(())
}

/// `pecu id login verify` — decide whether to believe it.
pub fn verify(ui: &Ui, settings: &Settings, args: &LoginVerifyArgs) -> miette::Result<()> {
    // The replay check comes first — before the signature is even parsed, and
    // long before the node is touched. It needs neither, and a challenge this
    // machine never issued is refused whatever arrives alongside it.
    let path = record_path(settings, &args.challenge)?;
    if !args.stateless {
        let issued = load_issued(&path)?.ok_or(LoginError::UnknownChallenge)?;
        if issued.audience != args.audience {
            return Err(LoginError::WrongAudience {
                issued: issued.audience,
                given: args.audience.clone(),
            }
            .into());
        }
    }

    let signature = IdentitySignature::from_base64(args.signature.trim())
        .map_err(|source| LoginError::BadSignature { source })?;

    let node = node::connect(&settings.profile)?;
    let request = LoginRequest {
        audience: args.audience.clone(),
        challenge: args.challenge.clone(),
    };
    let policy = LoginPolicy {
        max_age_blocks: args
            .max_age
            .unwrap_or(LoginPolicy::default().max_age_blocks),
        ..LoginPolicy::default()
    };

    ui.sdk(format!(
        "verus_sdk::network::verify_login(&node, {:?}, &signature, &request, &policy)",
        args.name
    ));
    let logged_in = verify_login(&node, &args.name, &signature, &request, &policy)
        .map_err(|source| flow("verifying the signature", source))?;
    ui.sdk_result(format!(
        "LoggedIn {{ name: {}, signers: {} }}",
        logged_in.name,
        logged_in.signers.len()
    ));

    // Only once it has been believed. A challenge burned on a rejected attempt
    // would let anyone who can guess one deny a real login by spending it.
    if !args.stateless {
        consume(&path)?;
    }

    if ui.is_json() {
        emit(&serde_json::json!({
            "verified": true,
            "name": logged_in.name,
            "identity_address": logged_in.identity_address,
            "signed_at": logged_in.signed_at,
            "signers": logged_in.signers.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "audience": args.audience,
            "challenge": args.challenge,
            "replay_checked": !args.stateless,
            "max_age_blocks": policy.max_age_blocks,
        }));
        return Ok(());
    }

    ui.panel(&verified_panel(ui, &logged_in, args, &policy));
    ui.explain_panel();
    Ok(())
}

fn verified_panel(
    ui: &Ui,
    logged_in: &LoggedIn,
    args: &LoginVerifyArgs,
    policy: &LoginPolicy,
) -> Panel {
    let palette = ui.theme.palette;
    let glyphs = ui.theme.glyphs;

    let mut panel = Panel::new("VERIFIED")
        .row(
            "identity",
            Text::of(
                fmt::untrusted(&logged_in.name, NAME_BUDGET, glyphs.ellipsis),
                palette.accent,
            ),
        )
        .row(
            "address",
            Text::of(&logged_in.identity_address, palette.value),
        )
        .row(
            "audience",
            Text::of(
                fmt::untrusted(&args.audience, AUDIENCE_BUDGET, glyphs.ellipsis),
                palette.value,
            ),
        )
        .row(
            "signed at",
            Text::of(fmt::height(logged_in.signed_at.into()), palette.value).push(
                format!("  (within {} blocks)", policy.max_age_blocks),
                palette.muted,
            ),
        );

    for (index, signer) in logged_in.signers.iter().enumerate() {
        // Named individually rather than counted: for an m-of-n identity,
        // *which* keys signed is the interesting part.
        let label = if index == 0 { "signed by" } else { "" };
        panel = panel.row(label, Text::of(signer.to_string(), palette.value));
    }

    panel = panel.line(Text::of(glyphs.ok, palette.ok).space().push(
        if args.stateless {
            "the signature is good"
        } else {
            "the signature is good, and this challenge is now spent"
        },
        palette.ok,
    ));

    if args.stateless {
        panel = panel.note(Text::of(
            "--stateless: nothing here checked that the challenge was fresh or unused. The \
             same signature will verify again, for as long as its height is within the age \
             bound",
            palette.warn,
        ));
    }
    panel.note(Text::of(
        "checked against the identity as it stood at that height, not as it stands now — but \
         a revocation since then would have rejected it anyway",
        palette.muted,
    ))
}

/// Where a challenge's record lives.
///
/// The challenge names the file, so it is checked against anything that could
/// escape the directory before it is used as one. `id login challenge` only
/// ever issues hex, but `verify` takes whatever it is handed.
fn record_path(settings: &Settings, challenge: &str) -> Result<PathBuf, LoginError> {
    let safe = !challenge.is_empty()
        && challenge.len() <= 128
        && challenge
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-');
    if !safe {
        return Err(LoginError::BadChallenge);
    }
    Ok(settings
        .paths
        .logins_dir()
        .join(format!("{challenge}.json")))
}

fn save_issued(path: &PathBuf, issued: &Issued) -> Result<(), LoginError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| LoginError::Store {
            path: parent.display().to_string(),
            advice: "check the permissions on your config directory".to_string(),
            source,
        })?;
    }
    let json = serde_json::to_string_pretty(issued).expect("plain data");
    std::fs::write(path, json).map_err(|source| LoginError::Store {
        path: path.display().to_string(),
        advice: "the challenge could not be recorded, so verifying it later would not be able \
                 to tell a replay from a fresh login"
            .to_string(),
        source,
    })
}

fn load_issued(path: &PathBuf) -> Result<Option<Issued>, LoginError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(LoginError::Store {
                path: path.display().to_string(),
                advice: "the record exists but could not be read; refusing rather than treating \
                         an unreadable challenge as a valid one"
                    .to_string(),
                source,
            })
        }
    };
    // A corrupt record is refused, not ignored. Falling through to "no such
    // challenge" would be the same answer, but reaching it by accident.
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|source| LoginError::Store {
            path: path.display().to_string(),
            advice: "this file is not a challenge record this version understands".to_string(),
            source: std::io::Error::new(std::io::ErrorKind::InvalidData, source),
        })
}

fn consume(path: &PathBuf) -> Result<(), LoginError> {
    std::fs::remove_file(path).map_err(|source| LoginError::Store {
        path: path.display().to_string(),
        advice: "the login was good but the challenge could not be marked as used, so the same \
                 signature would be accepted again"
            .to_string(),
        source,
    })
}

fn choose_key(
    store: &Keystore,
    label: Option<&str>,
) -> Result<crate::keystore::Envelope, miette::Report> {
    if let Some(label) = label {
        return Ok(store.load(label)?);
    }
    let keys = store.list()?;
    match keys.len() {
        0 => Err(LoginError::NoKey.into()),
        1 => Ok(store.load(&keys[0].label)?),
        count => Err(LoginError::AmbiguousKey { count }.into()),
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

fn emit(value: &serde_json::Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).expect("the report is plain data")
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Paths;

    fn settings_at(root: &std::path::Path) -> Settings {
        Settings::resolve_in(Paths::at(root), None, None).expect("defaults")
    }

    #[test]
    fn a_challenge_cannot_name_a_file_outside_the_store() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let settings = settings_at(dir.path());
        // `verify` takes the challenge from the command line, so this is the
        // one place an attacker-supplied string becomes a path.
        for hostile in [
            "../../etc/passwd",
            "..",
            "a/b",
            "a\\b",
            "",
            "with space",
            "nul\0byte",
        ] {
            assert!(
                record_path(&settings, hostile).is_err(),
                "{hostile:?} was accepted as a challenge"
            );
        }
    }

    #[test]
    fn an_issued_challenge_round_trips_and_is_consumed_once() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let settings = settings_at(dir.path());
        let path = record_path(&settings, "deadbeef").expect("valid");

        save_issued(
            &path,
            &Issued {
                audience: "https://example.com".into(),
                issued_at: 1,
            },
        )
        .expect("writable");

        let found = load_issued(&path).expect("readable").expect("present");
        assert_eq!(found.audience, "https://example.com");

        consume(&path).expect("removable");
        // The whole point: the second look finds nothing, so the same signature
        // presented again is refused.
        assert!(load_issued(&path).expect("readable").is_none());
    }

    #[test]
    fn a_corrupt_record_is_refused_rather_than_read_as_absent() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let settings = settings_at(dir.path());
        let path = record_path(&settings, "deadbeef").expect("valid");
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("writable");
        std::fs::write(&path, "{ not json").expect("writable");

        // "Absent" and "unreadable" lead to the same refusal today, but for
        // different reasons, and only one of them is a bug worth seeing.
        assert!(load_issued(&path).is_err());
    }

    #[test]
    fn the_signed_message_binds_the_audience_to_the_challenge() {
        // The SDK length-prefixes both fields so one cannot be shifted into the
        // other. Worth an assertion here because this app chooses what goes in
        // them, and a wallet that let those blur would accept a signature made
        // for a different site.
        let one = LoginRequest {
            audience: "https://example.com/a".into(),
            challenge: "1".into(),
        };
        let two = LoginRequest {
            audience: "https://example.com/".into(),
            challenge: "a1".into(),
        };
        assert_ne!(one.message(), two.message());
    }
}
