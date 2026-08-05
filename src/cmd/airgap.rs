//! `pecu plan send` · `pecu sign` · `pecu broadcast` — the air-gap ceremony.
//!
//! Three commands because there are three machines' worth of trust:
//!
//! 1. **`plan send`** runs where the node is. It holds no key and cannot sign;
//!    it chooses coins and places outputs, and hands over a partial transaction.
//! 2. **`sign`** runs where the key is. It opens no socket — it does not need
//!    one, because everything it must check is inside the bytes it was given.
//! 3. **`broadcast`** runs where the node is again, and only carries finished
//!    bytes.
//!
//! # What the signer has to check, and why it can
//!
//! Whoever planned the transaction chose the outputs, and a signature is the
//! irreversible step. So `sign` shows the whole thing before it will sign
//! anything, and it checks the sighash types: outputs are only binding on your
//! input if your input commits to them. Under `SIGHASH_NONE` they are not
//! covered at all and whoever holds the partial can redirect the money after
//! you sign. That check is [`Summary::commits_to_all_outputs`], and this refuses
//! to sign without `--yes` when it is false.
//!
//! The planning and signing halves go through the same `plan_transparent_send`
//! the one-shot `pecu send` uses, so the transaction that comes out the far end
//! is byte-for-byte the one `send` would have made.

use std::io::{IsTerminal, Write};

use miette::Diagnostic;
use thiserror::Error;
use verus_sdk::cosign::{PartialTransaction, Summary};
use verus_sdk::money::Amount;
use verus_sdk::network::{broadcast as submit, prepare_unsigned_send, FlowError};
use verus_sdk::verus_keys::Address;

use crate::cli::{BroadcastArgs, Globals, PlanSendArgs, QrOut, SignArgs};
use crate::cmd::wallet;
use crate::config::Settings;
use crate::keystore::{self, Keystore};
use crate::node;
use crate::payload;
use crate::ui::{fmt, qr, Panel, Text, Ui};

#[derive(Debug, Error, Diagnostic)]
pub enum AirgapError {
    #[error("`{amount}` is not an amount")]
    #[diagnostic(
        code(pecu::bad_amount),
        help("a decimal number of coins, at most eight places")
    )]
    BadAmount { amount: String },

    #[error("that is not a partial transaction")]
    #[diagnostic(code(pecu::bad_plan), help("{detail}"))]
    BadPlan { detail: String },

    #[error("`{address}` is not a Verus address")]
    #[diagnostic(code(pecu::bad_address), help("transparent addresses start with R"))]
    BadAddress { address: String },

    #[error("no key to sign with")]
    #[diagnostic(
        code(pecu::no_key),
        help("pass --key <label>, or make one with `pecu key gen`")
    )]
    NoKey,

    #[error("the keystore holds {count} keys, so there is no obvious one to sign with")]
    #[diagnostic(code(pecu::ambiguous_key), help("name one with --key <label>"))]
    AmbiguousKey { count: usize },

    #[error("this key signed nothing — it does not unlock any of these inputs")]
    #[diagnostic(
        code(pecu::wrong_key),
        help("the plan spends outputs belonging to a different address. `pecu key list` shows what you hold")
    )]
    WrongKey,

    #[error("the inputs do not commit to the outputs")]
    #[diagnostic(
        code(pecu::partial_sighash),
        help("at least one input is signed under something other than SIGHASH_ALL, so the outputs shown are not what your signature protects — whoever holds this can redirect the money after you sign. Pass --yes only if you meant this")
    )]
    NotCommitted,

    #[error("still unsigned: {missing} of {total} input(s) have no signature")]
    #[diagnostic(
        code(pecu::incomplete),
        help("this needs another signer. Hand the partial on rather than broadcasting it")
    )]
    Incomplete { missing: usize, total: usize },

    #[error("cancelled")]
    #[diagnostic(code(pecu::cancelled), help("nothing was signed"))]
    Cancelled,

    #[error("cannot ask for confirmation")]
    #[diagnostic(
        code(pecu::no_tty),
        help("there is no terminal to confirm on — pass --yes once you have read the summary")
    )]
    CannotConfirm,

    #[error("{what} failed")]
    #[diagnostic(code(pecu::flow_failed), help("{advice}"))]
    Flow {
        what: &'static str,
        advice: String,
        #[source]
        source: Box<FlowError>,
    },

    #[error("the transaction could not be assembled")]
    #[diagnostic(code(pecu::finalize_failed), help("{detail}"))]
    Finalize { detail: String },
}

fn flow(what: &'static str, source: FlowError) -> AirgapError {
    AirgapError::Flow {
        what,
        advice: "run `pecu doctor`, or point somewhere else with --node".to_string(),
        source: Box::new(source),
    }
}

// ── plan ────────────────────────────────────────────────────────────────────

/// Build an unsigned transaction. Holds no key and cannot sign.
pub fn plan_send(
    ui: &Ui,
    settings: &Settings,
    globals: &Globals,
    args: &PlanSendArgs,
) -> miette::Result<()> {
    let outcome = plan_send_inner(ui, settings, globals, args);
    if !ui.is_json() {
        ui.explain_panel();
    }
    outcome
}

fn plan_send_inner(
    ui: &Ui,
    settings: &Settings,
    _globals: &Globals,
    args: &PlanSendArgs,
) -> miette::Result<()> {
    let amount = Amount::from_coins_str(&args.amount).map_err(|_| AirgapError::BadAmount {
        amount: args.amount.clone(),
    })?;

    let node = node::connect(&settings.profile)?;

    // The same resolution the read-only commands use: this step is watch-only,
    // so a stored key contributes its address and nothing else. A VerusID name
    // resolves here too, which is what makes planning a spend *from* an
    // identity possible without looking its i-address up by hand.
    let from = wallet::resolve_address(
        ui,
        &node,
        settings,
        args.target.address.as_deref(),
        args.target.key.as_deref(),
    )?;
    let from = from.address;
    let from: Address = from.parse().map_err(|_| AirgapError::BadAddress {
        address: from.clone(),
    })?;
    ui.sdk(format!(
        "verus_sdk::network::prepare_unsigned_send(&node, &{from}, {:?}, Amount::from_coins_str({:?}))",
        args.to,
        amount.to_coins_string()
    ));
    let partial = prepare_unsigned_send(&node, &from, &args.to, amount)
        .map_err(|source| flow("planning the payment", source))?;
    ui.sdk(format!(
        "PartialTransaction with {} input(s), {} output(s)",
        partial.inputs.len(),
        partial.outputs.len()
    ));

    let bytes = partial.to_bytes().map_err(|error| AirgapError::BadPlan {
        detail: error.to_string(),
    })?;
    let encoded = hex::encode(&bytes);

    if let Some(path) = &args.out {
        payload::write_hex(path, &encoded)?;
    }

    hand_over(ui, &args.qr, &encoded, "unsigned plan")?;

    if ui.is_json() {
        emit(&serde_json::json!({
            "kind": "plan",
            "from": from.to_string(),
            "to": args.to,
            "plan": encoded,
            "inputs": partial.inputs.len(),
            "outputs": partial.outputs.len(),
            "bytes": bytes.len(),
            "written_to": args.out,
        }));
        return Ok(());
    }

    let palette = ui.theme.palette;
    ui.panel(&summary_panel(
        ui,
        settings,
        "PLAN",
        &partial,
        Some(&from.to_string()),
    )?);
    ui.blank();
    let mut carried = Panel::new("UNSIGNED PLAN")
        .wrapped(0, Text::of(&encoded, palette.value))
        .note(Text::of(
            "no key was used and none is needed here — this cannot be broadcast until it is signed",
            palette.muted,
        ));
    if let Some(path) = &args.out {
        carried = carried.path("written to", path);
    }
    ui.panel(&carried);
    Ok(())
}

// ── sign ────────────────────────────────────────────────────────────────────

/// Sign a plan. Opens no socket.
pub fn sign(
    ui: &Ui,
    settings: &Settings,
    globals: &Globals,
    args: &SignArgs,
) -> miette::Result<()> {
    let bytes = if args.qr_in.is_empty() {
        payload::read_hex(args.input.as_deref())?
    } else {
        from_qr(&args.qr_in)?
    };
    let mut partial =
        PartialTransaction::from_bytes(&bytes).map_err(|error| AirgapError::BadPlan {
            detail: error.to_string(),
        })?;

    let summary = partial.summary().map_err(|error| AirgapError::BadPlan {
        detail: error.to_string(),
    })?;

    if !ui.is_json() {
        ui.panel(&summary_panel(
            ui,
            settings,
            "ABOUT TO SIGN",
            &partial,
            None,
        )?);
    }

    // The one check a co-signer cannot make by eye. Outputs are binding only if
    // every input commits to them; anything else and the money can be
    // redirected after the signature exists.
    if !summary.commits_to_all_outputs() && !globals.yes {
        return Err(AirgapError::NotCommitted.into());
    }
    if !globals.yes && !ui.is_json() {
        confirm(ui)?;
    }

    let store = Keystore::new(&settings.paths);
    let envelope = match args.key.as_deref() {
        Some(label) => store.load(label)?,
        None => {
            let keys = store.list()?;
            match keys.len() {
                0 => return Err(AirgapError::NoKey.into()),
                1 => keys.into_iter().next().expect("just checked"),
                count => return Err(AirgapError::AmbiguousKey { count }.into()),
            }
        }
    };
    let secret = keystore::passphrase(&format!("passphrase for `{}`", envelope.label), false)?;
    let key = envelope.unlock(&secret)?;

    ui.sdk("partial.sign(&key)");
    let signed_count = partial.sign(&key).map_err(|error| AirgapError::BadPlan {
        detail: error.to_string(),
    })?;
    ui.sdk_result(format!("{signed_count} input(s) signed"));
    if signed_count == 0 {
        return Err(AirgapError::WrongKey.into());
    }

    // Not complete means another signer is still needed — hand on the partial,
    // not a transaction that cannot be mined.
    if !partial.is_complete() {
        let unsigned = partial
            .summary()
            .map(|s| s.signatures_per_input.iter().filter(|n| **n == 0).count())
            .unwrap_or(0);
        let encoded = hex::encode(partial.to_bytes().map_err(|error| AirgapError::BadPlan {
            detail: error.to_string(),
        })?);
        if let Some(path) = &args.out {
            payload::write_hex(path, &encoded)?;
        }
        if ui.is_json() {
            emit(&serde_json::json!({
                "kind": "partially_signed",
                "complete": false,
                "signed_inputs": signed_count,
                "partial": encoded,
            }));
        } else {
            ui.blank();
            ui.panel(
                &Panel::new("PARTIALLY SIGNED")
                    .wrapped(0, Text::of(&encoded, ui.theme.palette.value))
                    .note(Text::of(
                        "another signature is still needed — pass this on, do not broadcast it",
                        ui.theme.palette.warn,
                    )),
            );
        }
        return Err(AirgapError::Incomplete {
            missing: unsigned,
            total: partial.inputs.len(),
        }
        .into());
    }

    ui.sdk("partial.finalize()");
    let finished = partial.finalize().map_err(|error| AirgapError::Finalize {
        detail: error.to_string(),
    })?;
    ui.sdk_result(format!("SignedTransaction {{ txid: {} }}", finished.txid));

    if let Some(path) = &args.out {
        payload::write_hex(path, &finished.hex)?;
    }

    hand_over(ui, &args.qr, &finished.hex, "signed transaction")?;

    if ui.is_json() {
        emit(&serde_json::json!({
            "kind": "signed",
            "complete": true,
            "txid": finished.txid,
            "hex": finished.hex,
            "fee": finished.fee.to_sat(),
            "written_to": args.out,
        }));
        return Ok(());
    }

    let palette = ui.theme.palette;
    ui.blank();
    let mut done = Panel::new("SIGNED TRANSACTION")
        .row("txid", Text::of(&finished.txid, palette.accent))
        .rule()
        .wrapped(0, Text::of(&finished.hex, palette.value))
        .note(Text::of(
            "carry this back to a machine with a node and run `pecu broadcast`",
            palette.muted,
        ));
    if let Some(path) = &args.out {
        done = done.path("written to", path);
    }
    ui.panel(&done);
    ui.explain_panel();
    Ok(())
}

// ── broadcast ───────────────────────────────────────────────────────────────

/// Hand finished bytes to a node. Carries no key.
pub fn broadcast(
    ui: &Ui,
    settings: &Settings,
    globals: &Globals,
    args: &BroadcastArgs,
) -> miette::Result<()> {
    let bytes = if args.qr_in.is_empty() {
        payload::read_hex(args.input.as_deref())?
    } else {
        from_qr(&args.qr_in)?
    };
    let encoded = hex::encode(&bytes);

    // Decoded locally first, so the last thing before the network is still a
    // description of what is about to be sent rather than an opaque blob.
    let transaction =
        verus_sdk::verus_wire::TxV4::deserialize(&bytes).map_err(|error| AirgapError::BadPlan {
            detail: format!("not a finished transaction: {error}"),
        })?;
    let txid = transaction
        .txid()
        .map(|mut id| {
            id.reverse();
            hex::encode(id)
        })
        .map_err(|error| AirgapError::BadPlan {
            detail: error.to_string(),
        })?;

    if !ui.is_json() {
        let palette = ui.theme.palette;
        ui.panel(
            &Panel::new("ABOUT TO BROADCAST")
                .row("txid", Text::of(&txid, palette.accent))
                .row(
                    "outputs",
                    Text::of(
                        fmt::plural(transaction.outputs.len(), "output", "outputs"),
                        palette.value,
                    ),
                )
                .row(
                    "value",
                    Text::of(
                        fmt::sats(transaction.outputs.iter().map(|o| o.value).sum::<u64>()),
                        palette.value,
                    )
                    .space()
                    .push(&settings.profile.currency, palette.muted),
                ),
        );
        if !globals.yes {
            confirm(ui)?;
        }
    }

    let node = node::connect(&settings.profile)?;
    ui.sdk(format!(
        "verus_sdk::network::broadcast(&node, <{} bytes>, {txid:?})",
        bytes.len()
    ));
    let accepted = submit(&node, &encoded, &txid).map_err(|source| flow("broadcasting", source))?;
    ui.sdk_result(format!("txid {accepted}"));

    if ui.is_json() {
        emit(&serde_json::json!({ "kind": "broadcast", "txid": accepted }));
        return Ok(());
    }
    ui.blank();
    ui.ok(format!("broadcast — txid {accepted}"));
    ui.note(format!(
        "{}/tx/{accepted}",
        settings.profile.explorer.trim_end_matches('/')
    ));
    ui.explain_panel();
    Ok(())
}

// ── shared ──────────────────────────────────────────────────────────────────

/// What this transaction does, as the SDK's own [`Summary`] describes it.
fn summary_panel(
    ui: &Ui,
    settings: &Settings,
    title: &str,
    partial: &PartialTransaction,
    from: Option<&str>,
) -> Result<Panel, AirgapError> {
    let summary = partial.summary().map_err(|error| AirgapError::BadPlan {
        detail: error.to_string(),
    })?;
    let palette = ui.theme.palette;
    let glyphs = ui.theme.glyphs;
    let currency = &settings.profile.currency;

    let mut panel = Panel::new(title);
    if let Some(from) = from {
        panel = panel.row("from", Text::of(from, palette.value));
    }
    panel = panel
        .row(
            "spending",
            Text::of(fmt::amount(summary.total_in), palette.value)
                .space()
                .push(currency, palette.muted)
                .space()
                .push(
                    format!(
                        "across {}",
                        fmt::plural(partial.inputs.len(), "input", "inputs")
                    ),
                    palette.muted,
                ),
        )
        .row(
            "paying out",
            Text::of(fmt::amount(summary.total_out), palette.accent)
                .space()
                .push(currency, palette.muted),
        )
        .row(
            "fee and burn",
            Text::of(fmt::amount(summary.fee_and_burn), palette.value)
                .space()
                .push(currency, palette.muted),
        )
        .row("expiry", expiry_text(ui, partial));

    panel = panel.row("commits", commitment(ui, &summary));

    panel = panel.section("OUTPUTS");
    for (index, (amount, address)) in summary.outputs.iter().enumerate() {
        // Built as spans, never as one Text's `render()` pushed into another:
        // the escapes in a rendered string are counted as visible width, and the
        // frame comes out ragged. The kit exists to make that impossible; going
        // around it puts it straight back.
        let (destination, style) = match address {
            Some(address) => (address.to_string(), palette.value),
            // `Summary` decodes plain key-hash outputs only. Anything else is a
            // CryptoCondition it will not guess at, and neither will this.
            None => (
                "a CryptoCondition — read the script before signing".to_string(),
                palette.warn,
            ),
        };
        panel = panel
            .line(
                Text::of(format!("#{index}"), palette.muted)
                    .space()
                    .push(fmt::amount(*amount), palette.accent)
                    .space()
                    .push(currency, palette.muted),
            )
            .wrapped(
                5,
                Text::of(glyphs.arrow, palette.muted)
                    .space()
                    .push(destination, style),
            );
    }
    Ok(panel)
}

/// The sighash check, spelled out. This is the difference between "these are the
/// outputs" and "these are the outputs your signature protects".
fn commitment(ui: &Ui, summary: &Summary) -> Text {
    let palette = ui.theme.palette;
    let glyphs = ui.theme.glyphs;
    if summary.commits_to_all_outputs() {
        Text::of(glyphs.ok, palette.ok).space().push(
            "every input covers every output (SIGHASH_ALL)",
            palette.value,
        )
    } else {
        Text::of(glyphs.danger, palette.danger).space().push(
            "NOT every input covers every output — the money can be redirected after you sign",
            palette.danger,
        )
    }
}

fn expiry_text(ui: &Ui, partial: &PartialTransaction) -> Text {
    let palette = ui.theme.palette;
    match partial.expiry.to_height() {
        0 => Text::of(ui.theme.glyphs.warn, palette.warn)
            .space()
            .push("never — this stays minable forever", palette.warn),
        height => Text::of(
            format!("height {}", fmt::height(height.into())),
            palette.value,
        ),
    }
}

fn confirm(ui: &Ui) -> Result<(), AirgapError> {
    if !std::io::stdin().is_terminal() {
        return Err(AirgapError::CannotConfirm);
    }
    ui.blank();
    print!("  type `yes` to continue: ");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .map_err(|_| AirgapError::CannotConfirm)?;
    if answer.trim() != "yes" {
        return Err(AirgapError::Cancelled);
    }
    Ok(())
}

/// Draw and/or write the payload as QR codes, if asked.
///
/// Chunked and numbered, so several frames reassemble in any order — see
/// [`crate::ui::qr`]. Nothing happens unless a flag asked for it: a wall of
/// block characters is not what most runs want.
fn hand_over(ui: &Ui, options: &QrOut, hex: &str, what: &str) -> Result<(), miette::Report> {
    if !options.qr && options.qr_out.is_none() {
        return Ok(());
    }
    let frames = qr::frames(hex);
    let palette = ui.theme.palette;

    // Drawing is display and belongs to the rendered path only; writing a PNG
    // is a side effect the caller asked for and happens either way. `--json
    // --qr-out` silently producing no file was a bug.
    if options.qr && !ui.is_json() {
        for (index, frame) in frames.iter().enumerate() {
            ui.blank();
            ui.line(Text::of(
                format!("{what} — frame {} of {}", index + 1, frames.len()),
                palette.label,
            ));
            print!("{}", qr::render(frame)?);
        }
    }

    if let Some(stem) = &options.qr_out {
        let mut written = Vec::new();
        for (index, frame) in frames.iter().enumerate() {
            let path = numbered(stem, index + 1);
            qr::write_png(&path, frame)?;
            written.push(path);
        }
        if !ui.is_json() {
            ui.blank();
            let mut panel = Panel::new("QR FRAMES");
            for path in &written {
                panel = panel.path("wrote", path);
            }
            ui.panel(&panel.note(Text::of(
                "every frame is needed; they reassemble in any order",
                palette.muted,
            )));
        }
    }
    Ok(())
}

/// `plan.png` with frame 2 of 3 becomes `plan-2.png`.
fn numbered(stem: &std::path::Path, index: usize) -> std::path::PathBuf {
    let extension = stem
        .extension()
        .map(|ext| ext.to_string_lossy().to_string())
        .unwrap_or_else(|| "png".to_string());
    stem.with_extension("").with_file_name(format!(
        "{}-{index}.{extension}",
        stem.with_extension("")
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "frame".to_string())
    ))
}

/// Read a payload from QR codes in one or more PNGs.
fn from_qr(paths: &[std::path::PathBuf]) -> Result<Vec<u8>, miette::Report> {
    let mut frames = Vec::new();
    for path in paths {
        frames.extend(qr::read_png(path)?);
    }
    let hex = qr::reassemble(&frames)?;
    Ok(crate::payload::read_hex(Some(&hex))?)
}

fn emit(value: &serde_json::Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).expect("plain data")
    );
}
