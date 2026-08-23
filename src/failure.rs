//! What a failure looks like on the way out of the process — and the one place
//! a JSON document reaches stdout.
//!
//! Every command answers with a [`miette::Result`], and for a long time the
//! only thing that met the `Err` half was std's blanket `Termination` impl for
//! `Result`: it printed `Error: {report:?}` to stderr and returned 1. That is
//! a good report for a person and nothing at all for a script. Under `--json`
//! stdout was empty, so `pecu … --json | jq` failed by printing nothing and
//! exiting 0, and the only handle on *what* went wrong was the `pecu::…` code
//! sitting inside prose whose wrap width and help text are free to change.
//!
//! So this module does the three things std cannot. It knows whether `--json`
//! was asked for, and prints the same `pecu::…` code as a field rather than as
//! prose; it answers with an exit code that says which *kind* of failure this
//! was, so a script can tell "the endpoint never answered" from "the node
//! answered and the answer was no" without reading either stream; and it owns
//! the run's document, so "exactly one document on stdout" is a property of
//! the program rather than a rule every command has to remember.
//!
//! The human report on stderr is unchanged, byte for byte. All of this is
//! additive.

use std::io::Write;
use std::process::ExitCode;
use std::sync::{Mutex, MutexGuard};

use miette::Diagnostic;
use serde_json::{json, Value};
use verus_sdk::network::{FlowError, RpcError};

/// The exit codes `pecu` answers with.
///
/// Deliberately few. There are 104 distinct `pecu::…` diagnostic codes in this
/// binary and a code per diagnostic would be a contract nobody could keep — the
/// diagnostic code is already the fine-grained discriminator, and it now
/// travels in the JSON. What an exit status has to answer is the coarser
/// question a shell script branches on: is this worth retrying?
///
/// `2` is absent on purpose: clap already exits 2 for a usage error, before any
/// of this runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum Exit {
    /// The request was understood and the answer was no. Retrying changes
    /// nothing: a refusal, a bad argument, a missing key, a daemon that
    /// answered with an error code.
    Refused = 1,
    /// The endpoint did not answer the question. Nothing came back, or what
    /// came back was not an answer this build can use — so nothing was learned
    /// and nothing happened. Retrying, or pointing `--node` somewhere else, is
    /// the remedy.
    Unreachable = 3,
    /// The outcome is genuinely unknown, which for a broadcast means the
    /// transaction may or may not be propagating. **Not** retry-safe: resending
    /// blindly risks a second broadcast of something already in flight. The
    /// document on stdout carries the txid and the signed hex to check with.
    Uncertain = 4,
}

/// The document this run prints, held until the process is on its way out.
///
/// Deferred rather than printed where it is built, because the failing paths
/// are the reason this module exists. Three commands print a document *and*
/// fail — `doctor`, whose local half is worth having when the node is down;
/// `send`, whose signed hex cannot be recovered afterwards; and `sign`, whose
/// partial is what a co-signer has to be handed — and in each the error object
/// belongs *inside* that document rather than after it as a second one.
///
/// Holding the document here makes that structural. The alternative, tried
/// first, was a flag each such command set as it printed: it worked for the two
/// commands whoever wrote it thought of, and `sign` silently printed two
/// documents. A command cannot opt out of this one by forgetting it exists.
static PENDING: Mutex<Option<Value>> = Mutex::new(None);

/// A poisoned lock means a thread panicked while holding it. The document is
/// still whatever it was, and dropping it on the floor here would be a worse
/// answer than printing it.
fn pending() -> MutexGuard<'static, Option<Value>> {
    PENDING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Hand over the document this run answers with. Printed by [`flush`] on the
/// way out, or folded into the failure by [`finish`].
pub fn document(value: &Value) {
    debug_assert!(value.is_object(), "a document is an object: {value}");
    let mut pending = pending();
    debug_assert!(
        pending.is_none(),
        "a run prints at most one document, and every emitter returns straight after emitting"
    );
    *pending = Some(value.clone());
}

/// Print the document a successful run built, if it built one.
pub fn flush() {
    let document = pending().take();
    if let Some(document) = document {
        print(&document);
    }
}

/// The one write to stdout.
///
/// `writeln!` rather than `println!` so a consumer that stops reading —
/// `pecu … --json | head -1`, `| jq -e …`, `| grep -q …` — gets the documented
/// exit code back instead of a panic and a `101` on a broken pipe.
fn print(value: &Value) {
    let rendered = serde_json::to_string_pretty(value).expect("the document is plain data");
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{rendered}");
}

/// The one shape a `pecu` failure has in JSON.
///
/// `code` is the `#[diagnostic(code(pecu::…))]` string verbatim — the same
/// token stderr prints after `Error:`, not a second naming scheme derived from
/// it. `message` and `help` are the `#[error]` and `#[help]` text. `causes` is
/// the `#[source]` chain, which is where the SDK's own wording lives: the
/// rendered report shows it under `╰─▶`, and without it the JSON would say
/// strictly less than the prose it replaces.
///
/// Every key is always present. `code` and `help` are null when the diagnostic
/// carries none, and `causes` is an empty array rather than absent, so a
/// consumer can index without a shape check.
pub fn object(error: &(dyn Diagnostic + 'static)) -> Value {
    let mut causes = Vec::new();
    let mut cause = error.source();
    while let Some(current) = cause {
        causes.push(current.to_string());
        cause = current.source();
    }

    json!({
        "code": error.code().map(|code| code.to_string()),
        "message": error.to_string(),
        "help": error.help().map(|help| help.to_string()),
        "causes": causes,
    })
}

/// Print the failure and say how to exit.
///
/// `json` is `cli.globals.json`, which has to be read before `dispatch` takes
/// ownership of the `Cli`.
pub fn finish(report: miette::Report, json: bool) -> ExitCode {
    if json {
        // stdout, because that is where the machine-readable answer lives on
        // every succeeding path too. Nothing was written here before on the
        // paths that printed no document, so no consumer can be relying on the
        // emptiness.
        let pending = pending().take();
        print(&failed(pending, object(&*report)));
    } else {
        // Nothing can be pending without `--json`, since that is the only thing
        // a document is built for. Flushed anyway rather than silently dropped.
        flush();
    }

    // Byte for byte what std's `Termination for Result` printed while `main`
    // returned `miette::Result`. `writeln!` rather than `eprintln!` so a closed
    // stderr — `pecu … 2>&-`, or a reader that hung up — reports the exit code
    // instead of panicking on the way to it.
    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(stderr, "Error: {report:?}");

    ExitCode::from(classify(&*report) as u8)
}

/// The single document a failing `--json` run prints: whatever the command
/// built, with the error inside it, or the error on its own.
///
/// The command's own `error` wins if it set one. Only `send` does, building it
/// from the same diagnostic this is handed, and its tri-state is asserted
/// against that document rather than against this.
fn failed(pending: Option<Value>, error: Value) -> Value {
    // A non-object document cannot happen — `document` asserts otherwise — and
    // if it ever did, the error is what a script came for. Printing both would
    // trade a lost field for the broken pipeline this module exists to prevent.
    let mut envelope = pending
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    envelope
        .as_object_mut()
        .expect("just filtered to objects")
        .entry("error")
        .or_insert(error);
    envelope
}

/// Which kind of failure this is.
///
/// Read off the `#[source]` chain rather than off the diagnostic code, because
/// the code cannot answer this question: `pecu::flow_failed` alone covers both
/// a daemon that refused a transaction and a socket that never opened, and
/// `pecu::node_unreachable` is one diagnostic over every failed node request,
/// including the ones the node answered. The SDK types underneath do make the
/// distinction, and they are the ones that know.
fn classify(error: &(dyn Diagnostic + 'static)) -> Exit {
    let mut link: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(current) = link {
        if let Some(flow) = downcast::<FlowError>(current) {
            return match flow {
                FlowError::BroadcastUncertain { .. } => Exit::Uncertain,
                FlowError::Rpc(rpc) => from_rpc(rpc),
                _ => Exit::Refused,
            };
        }
        if let Some(rpc) = downcast::<RpcError>(current) {
            return from_rpc(rpc);
        }
        link = current.source();
    }
    Exit::Refused
}

/// One link of the chain, if it is an `E`.
///
/// The second attempt is not defensive padding. Six diagnostics in this tree
/// hold their SDK failure as `#[source] source: Box<FlowError>` — boxed to keep
/// the error variant small — and thiserror hands a boxed source back as
/// `&Box<E>`, not as `&E`. So a chain walker that only asks for `E` finds
/// nothing in exactly the cases this exists to classify.
fn downcast<'a, E: std::error::Error + 'static>(
    link: &'a (dyn std::error::Error + 'static),
) -> Option<&'a E> {
    link.downcast_ref::<E>()
        .or_else(|| link.downcast_ref::<Box<E>>().map(|boxed| &**boxed))
}

/// Only [`RpcError::Node`] is the daemon reading the request and answering it.
/// A refused connection, a proxy's HTML, a reply this build cannot parse and a
/// method the endpoint will not serve are all the same thing to a script: this
/// endpoint did not answer the question, nothing happened, and the remedy is to
/// try again or point `--node` somewhere else.
///
/// The rest — a reply over the configured ceiling, a number that could not be
/// read exactly, a cassette refusing a write — are not connection problems and
/// not retry-safe in any useful sense: running the same command again produces
/// the same answer until something local changes.
///
/// None of the [`Exit::Unreachable`] cases can reach here from a *broadcast*.
/// The SDK promotes a transport failure, a malformed reply and an unexpected
/// one to [`FlowError::BroadcastUncertain`], because the bytes may have gone
/// out before the connection broke, and it hands `-32601` back as a
/// [`RpcError::MethodUnavailable`] only after establishing that the transaction
/// was never relayed. That is what keeps [`Exit::Unreachable`] safe to retry.
fn from_rpc(rpc: &RpcError) -> Exit {
    match rpc {
        RpcError::Node { .. } => Exit::Refused,
        RpcError::Transport(_)
        | RpcError::Malformed(_)
        | RpcError::Unexpected(_)
        | RpcError::MethodUnavailable { .. } => Exit::Unreachable,
        _ => Exit::Refused,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use miette::Diagnostic;
    use thiserror::Error;

    #[derive(Debug, Error, Diagnostic)]
    #[error("nothing on this chain is called `ghost@`")]
    #[diagnostic(code(pecu::no_such_currency), help("try a name that exists"))]
    struct Plain;

    #[derive(Debug, Error, Diagnostic)]
    #[error("{what} failed")]
    #[diagnostic(code(pecu::flow_failed))]
    struct Wrapped {
        what: &'static str,
        #[source]
        source: Box<FlowError>,
    }

    fn wrapped(source: FlowError) -> Wrapped {
        Wrapped {
            what: "broadcasting",
            source: Box::new(source),
        }
    }

    #[test]
    fn the_object_carries_the_diagnostic_code_verbatim() {
        let document = object(&Plain);
        assert_eq!(document["code"], "pecu::no_such_currency");
        assert_eq!(
            document["message"],
            "nothing on this chain is called `ghost@`"
        );
        assert_eq!(document["help"], "try a name that exists");
        assert_eq!(document["causes"], json!([]));
    }

    #[test]
    fn a_diagnostic_without_help_still_has_the_key() {
        #[derive(Debug, Error, Diagnostic)]
        #[error("refused to print a private key without --yes")]
        #[diagnostic(code(pecu::export_refused))]
        struct NoHelp;

        let document = object(&NoHelp);
        assert_eq!(document["code"], "pecu::export_refused");
        assert!(document["help"].is_null(), "help should be present as null");
    }

    #[test]
    fn the_source_chain_becomes_causes() {
        let document = object(&wrapped(FlowError::Rpc(RpcError::Transport(
            "connection refused".into(),
        ))));
        let causes = document["causes"].as_array().expect("an array");
        assert_eq!(causes.len(), 1, "one link under the head: {causes:?}");
        assert!(
            causes[0].as_str().expect("a string").contains("transport"),
            "the SDK's own wording is what makes this worth carrying: {causes:?}"
        );
    }

    /// A failure with no document behind it prints the error and nothing else.
    #[test]
    fn a_failure_on_its_own_is_one_object_with_one_key() {
        let envelope = failed(None, object(&Plain));
        assert_eq!(envelope["error"]["code"], "pecu::no_such_currency");
        assert_eq!(
            envelope.as_object().expect("an object").len(),
            1,
            "nothing else invented: {envelope:#}"
        );
    }

    /// The case that used to print two documents. `sign` emits its partial and
    /// then fails; the error goes inside what it printed, and the partial — the
    /// hex a co-signer has to be handed — survives.
    #[test]
    fn a_document_that_was_built_before_the_failure_carries_it() {
        let envelope = failed(
            Some(json!({ "kind": "partially_signed", "partial": "deadbeef" })),
            object(&Plain),
        );
        assert_eq!(envelope["kind"], "partially_signed");
        assert_eq!(envelope["partial"], "deadbeef");
        assert_eq!(envelope["error"]["code"], "pecu::no_such_currency");
    }

    /// `send` builds its own from the same diagnostic, and its document is
    /// asserted against that. Overwriting it here would make two places the
    /// authority on one field.
    #[test]
    fn a_document_that_already_carries_an_error_keeps_it() {
        let envelope = failed(
            Some(json!({ "error": { "code": "pecu::flow_failed" } })),
            object(&Plain),
        );
        assert_eq!(envelope["error"]["code"], "pecu::flow_failed");
    }

    /// The split item 4 of #49 asks for, at the only layer that can make it: a
    /// daemon that answered `-26` refused the transaction, and a socket that
    /// never opened refused nothing.
    #[test]
    fn a_daemon_that_answered_is_not_an_unreachable_node() {
        assert_eq!(
            classify(&wrapped(FlowError::Rpc(RpcError::Node {
                code: -26,
                message: "16: bad-txns-inputs-spent".into(),
            }))),
            Exit::Refused
        );
        assert_eq!(
            classify(&wrapped(FlowError::Rpc(RpcError::Transport(
                "connection refused".into()
            )))),
            Exit::Unreachable
        );
    }

    /// An endpoint that is not a Verus node — a wrong port, a proxy, a captive
    /// portal — answers something, and it is not an answer. Calling that a
    /// refusal would tell a script the request was understood.
    #[test]
    fn an_answer_that_is_not_one_is_not_a_refusal() {
        assert_eq!(
            classify(&wrapped(FlowError::Rpc(RpcError::Malformed(
                "expected value at line 1 column 1".into()
            )))),
            Exit::Unreachable
        );
        assert_eq!(
            classify(&wrapped(FlowError::Rpc(RpcError::Unexpected(
                "no `result` field".into()
            )))),
            Exit::Unreachable
        );
        assert_eq!(
            classify(&wrapped(FlowError::Rpc(RpcError::MethodUnavailable {
                method: "sendrawtransaction",
            }))),
            Exit::Unreachable
        );
    }

    /// A ceiling this build was configured with, not a node that failed to
    /// answer. Running it again unchanged gets the same reply.
    #[test]
    fn a_reply_too_large_to_use_is_not_a_node_that_did_not_answer() {
        assert_eq!(
            classify(&wrapped(FlowError::Rpc(RpcError::ResponseTooLarge {
                cap: 8 * 1024 * 1024,
            }))),
            Exit::Refused
        );
    }

    #[test]
    fn an_unsettled_broadcast_is_neither() {
        assert_eq!(
            classify(&wrapped(FlowError::BroadcastUncertain {
                txid: "00".repeat(32),
                hex: "deadbeef".into(),
                reason: "the connection broke after the bytes went out".into(),
            })),
            Exit::Uncertain
        );
    }

    /// Nothing in the chain is an SDK error at all — a local refusal, a bad
    /// argument, a missing key. There is no node in the story, so there is
    /// nothing to retry.
    #[test]
    fn a_local_refusal_is_a_refusal() {
        assert_eq!(classify(&Plain), Exit::Refused);
    }

    #[test]
    fn the_exit_codes_are_the_documented_numbers() {
        assert_eq!(Exit::Refused as u8, 1);
        assert_eq!(Exit::Unreachable as u8, 3);
        assert_eq!(Exit::Uncertain as u8, 4);
    }
}
