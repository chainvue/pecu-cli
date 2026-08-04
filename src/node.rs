//! Talking to a public node.
//!
//! One place builds the client, so the timeout, the response ceiling and the
//! error wording are decided once. The SDK keeps reading and broadcasting in
//! separate traits — [`ChainReader`](verus_sdk::network::ChainReader) and
//! [`Broadcaster`](verus_sdk::network::Broadcaster) — and this returns the
//! concrete client that implements both; a command that must not spend takes
//! `&impl ChainReader` and is then incapable of it.

use std::time::Duration;

use miette::Diagnostic;
use thiserror::Error;
use verus_sdk::network::{HttpTransport, RpcClient, RpcError};

/// Long enough for a public node under load, short enough that a wrong URL
/// fails while you are still looking at the terminal.
const TIMEOUT: Duration = Duration::from_secs(20);

pub type Node = RpcClient<HttpTransport>;

#[derive(Debug, Error, Diagnostic)]
pub enum NodeError {
    #[error("`{url}` is not a usable node endpoint")]
    #[diagnostic(
        code(pecu::bad_endpoint),
        help("plaintext http:// is refused for anything but loopback, because every address you query would be readable in transit")
    )]
    Endpoint {
        url: String,
        // Boxed to keep the error variant small: this type is the `Err` half of
        // every networked call's Result.
        #[source]
        source: Box<RpcError>,
    },

    #[error("{what} failed against {url}")]
    #[diagnostic(code(pecu::node_unreachable), help("{advice}"))]
    Request {
        what: &'static str,
        url: String,
        advice: String,
        // Boxed to keep the error variant small: this type is the `Err` half of
        // every networked call's Result.
        #[source]
        source: Box<RpcError>,
    },
}

impl NodeError {
    /// Turn an [`RpcError`] into something that says what to do about it.
    pub fn request(what: &'static str, url: &str, source: RpcError) -> Self {
        let advice = match &source {
            RpcError::Transport(_) => {
                "check your connection, or point somewhere else with --node".to_string()
            }
            RpcError::MethodUnavailable { method } => format!(
                "the endpoint refused `{method}` — public nodes sit behind a method allowlist; \
                 try your own daemon with --node"
            ),
            RpcError::Node { code, .. } => format!("the daemon rejected the call with code {code}"),
            RpcError::Malformed(_) | RpcError::Unexpected(_) => {
                "the endpoint answered something this SDK build did not expect — it may not be a \
                 Verus node, or may be a different daemon version"
                    .to_string()
            }
            _ => "try another endpoint with --node".to_string(),
        };
        Self::Request {
            what,
            url: url.to_string(),
            advice,
            source: Box::new(source),
        }
    }
}

/// A read-and-broadcast client for `url`.
///
/// Builds no connection: the first request does that. So this succeeding means
/// the URL is well formed, not that anything is listening.
pub fn connect(url: &str) -> Result<Node, NodeError> {
    let transport = HttpTransport::new(url)
        .map_err(|source| NodeError::Endpoint {
            url: url.to_string(),
            source: Box::new(source),
        })?
        .with_timeout(TIMEOUT);
    Ok(RpcClient::new(transport))
}
