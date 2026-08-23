//! What a node can tell us about a currency's name, and what it cannot.
//!
//! A name is display text; the currency **id** is the part that identifies
//! anything. So a name is never the whole answer, and a lookup that failed is
//! never allowed to render as a currency that has no name. Both `wallet` and
//! `tx explain` ask the same question and both have to degrade the same way,
//! which is why the asking lives here rather than inside either of them.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use verus_sdk::network::{ChainReader, RpcError};
use verus_sdk::send::CurrencyId;
use verus_sdk::verus_keys::{Address, AddressKind};

use crate::config::Profile;
use crate::ui::fmt;

/// How long the whole naming step gets, however many currencies it covers.
///
/// A name is display text and the transaction is the answer, so the naming step
/// is worth roughly one request's worth of waiting — not one request's worth per
/// currency. Who chooses how many currencies there are matters here: `tx
/// explain` names every currency the bytes it was handed mention, and those
/// bytes come from a counterparty, so the count is not ours to bound. Against a
/// node that hangs, a full timeout each turned the default 20 seconds into over
/// three unbroken minutes for ten currencies, with nothing printed — on the one
/// command people reach for *because* something looks wrong.
///
/// A shared deadline rather than a cap on how many currencies are looked up:
/// what hurts is wall-clock, and a cap of N is still N timeouts against a
/// hanging node while dropping names a healthy node would have answered in
/// milliseconds. A request already in flight cannot be cut short from here, so
/// the real bound is this budget plus at most one more timeout — two requests'
/// worth in the worst case, instead of one per currency.
pub fn name_budget(profile: &Profile) -> Duration {
    Duration::from_secs(profile.timeout_secs)
}

/// What the node could tell us about one currency's name.
///
/// Three answers, not two. This was a `BTreeMap<CurrencyId, String>` where a
/// missing key meant both "the chain has no name for this" and "asking blew
/// up", and every render of it printed the same confident `(unnamed)`. A
/// lookup that failed is an **unknown**, which is the rule `wallet` already
/// applies to a balance and to the mempool; a name is not exempt from it.
#[derive(Debug)]
pub enum CurrencyName {
    /// The node answered, and this is the name it gave.
    Known(String),
    /// The node answered `-5` or `-8`: it knows no currency with this id. The
    /// one refusal that really is a statement about the currency — and note
    /// that it is a statement about the *currency*, not about its name. A node
    /// that has no such currency has not told us this currency is nameless.
    Absent,
    /// No name was got, and that says nothing about the currency. Deliberately
    /// not called "unreadable": this is the catch-all for every way the ask can
    /// fail, and most of them are not a garbled answer. A timeout, a refused
    /// connection or a node that does not serve the method all land here, and
    /// in none of them did an answer arrive to be read. The string is the
    /// reason, and it is carried rather than summarised so that `--json` can
    /// say what actually happened instead of guessing at a cause.
    Failed(String),
}

/// A verdict on each currency's name, asked one currency at a time.
///
/// `verus_sdk::network::currency_names` is the obvious call and is what this
/// used to be. It returns one `Result` for the whole set, so a single currency
/// the node describes in a way the SDK cannot parse discards every name already
/// collected — a wallet holding five tokens lost all five, and the panel said
/// `(unnamed)` about each of them. Underneath it is already one `getcurrency`
/// per currency, so asking here costs exactly the same round trips and keeps
/// the failures where they belong.
///
/// `currency_definition` rather than `currency`, which is what the SDK's
/// version calls: the same RPC read as a *definition* instead of a
/// registration policy, so it never parses the fee fields. A currency whose
/// `idimportfees` the daemon prints as `1e-8` is refused outright by the strict
/// number reader behind `currency`, and that one field is what blanked the
/// names in the first place. Read this way the name comes back.
///
/// `&impl ChainReader` rather than the concrete client: reading a name is the
/// one thing this does, and a reader cannot broadcast. `tx explain` is the
/// second caller and must never spend, so it is worth the type saying so.
///
/// One round trip per currency is only affordable while the whole step is
/// bounded, so `budget` bounds it — see [`name_budget`]. The deadline is
/// checked between lookups, and a currency it does not reach gets **no entry**
/// rather than a [`CurrencyName::Failed`]: nothing was asked about it, which is
/// a different fact from a lookup that was made and came back empty-handed. It
/// is the same absence as a currency nobody put on the list, and every renderer
/// here already knows what to say about that.
pub fn look_up_names(
    node: &impl ChainReader,
    wanted: &BTreeSet<CurrencyId>,
    budget: Duration,
) -> BTreeMap<CurrencyId, CurrencyName> {
    let deadline = Instant::now() + budget;
    wanted
        .iter()
        .take_while(|_| Instant::now() < deadline)
        .map(|currency| {
            let id = Address::new(AddressKind::Identity, currency.to_bytes()).to_string();
            let verdict = match node.currency_definition(&id) {
                Ok(summary) if summary.currency_id == id => CurrencyName::Known(summary.name),
                // Free consistency check, kept from the SDK's version: a node
                // that answers about a different currency than the one asked
                // about is confused or hostile, and either way its answer is
                // not a name for this token.
                Ok(other) => CurrencyName::Failed(format!(
                    "the node answered about {} instead",
                    other.currency_id
                )),
                // `-5` *and* `-8`, which is this repo's rule everywhere else
                // it asks a node whether something exists. It matters here more
                // than it looks: measured against api.verustest.net,
                // `getcurrency` answers a miss with `-8` and only `getidentity`
                // answers one with `-5`, so accepting `-5` alone would make
                // this arm unreachable against a real daemon and file every
                // genuinely unknown currency under a failed lookup instead.
                Err(RpcError::Node { code: -5 | -8, .. }) => CurrencyName::Absent,
                Err(error) => CurrencyName::Failed(error.to_string()),
            };
            (*currency, verdict)
        })
        .collect()
}

/// What `--explain` says a name lookup came back with.
///
/// `0 names` on its own reads as "the chain has no name for these", which is a
/// confident answer to a question that failed. Anything that was not answered
/// is counted out loud beside the names that were.
///
/// `wanted` is what was put on the list, and it is needed because the currencies
/// the budget never reached are the ones missing from `names` entirely. Left to
/// count only what came back, this would report a shortened run as a complete
/// one.
pub fn name_result(
    names: &BTreeMap<CurrencyId, CurrencyName>,
    wanted: &BTreeSet<CurrencyId>,
) -> String {
    let count =
        |matching: fn(&CurrencyName) -> bool| names.values().filter(|n| matching(n)).count();
    let absent = count(|n| matches!(n, CurrencyName::Absent));
    let failed = count(|n| matches!(n, CurrencyName::Failed(_)));
    let mut summary = fmt::plural(
        count(|n| matches!(n, CurrencyName::Known(_))),
        "name",
        "names",
    );
    if absent > 0 {
        summary.push_str(&format!(", {absent} the node has no currency for"));
    }
    if failed > 0 {
        summary.push_str(&format!(", {failed} the lookup failed for"));
    }
    let skipped = wanted.len().saturating_sub(names.len());
    if skipped > 0 {
        summary.push_str(&format!(", {skipped} the lookup ran out of time for"));
    }
    summary
}

/// One currency's name, as JSON.
///
/// An object rather than a bare string because there are three answers and not
/// two. `"name": null` said "this currency has no name" for a lookup that never
/// managed to ask, which is the same confident nothing the panel used to print;
/// the `known` here is about the *name*, the way the `known` around it is about
/// the balance.
///
/// Takes the verdict rather than the map, because a caller may have no map at
/// all: `tx explain` on offline hex never looked anything up, and `None` is
/// already the arm that says exactly that.
pub fn name_json(verdict: Option<&CurrencyName>) -> serde_json::Value {
    match verdict {
        Some(CurrencyName::Known(name)) => serde_json::json!({ "known": true, "name": name }),
        // `name: null` on its own reads as "this currency has no name", which
        // is not what the node said and is not true of a currency someone is
        // holding a balance in. The reason is carried so a consumer prints the
        // answer that was actually given.
        Some(CurrencyName::Absent) => serde_json::json!({
            "known": true,
            "name": null,
            "reason": "the node has no currency with this id",
        }),
        Some(CurrencyName::Failed(error)) => serde_json::json!({ "known": false, "error": error }),
        None => serde_json::json!({ "known": false, "error": "the name was not looked up" }),
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    use super::*;
    use crate::config::Profile;
    use crate::node::{self, Node};

    /// The chain's own currency on VRSCTEST.
    const NATIVE: &str = "iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq";

    /// A token registered on it.
    const TOKEN: &str = "iK2k8YH1jfR7RLmEZ3zac2Mkx5rxSgbMqg";

    /// Two more real VRSCTEST currencies, so the bound is measured over a set
    /// big enough that one timeout each would be unmistakable.
    const VETH: &str = "i9nwxtKuVYX4MSbeULLiK2ttVi6rUEhh4X";
    const BRIDGE: &str = "iSojYsotVzXz4wh2eJriASGo6UidJDDhL2";

    fn currency(i_address: &str) -> CurrencyId {
        CurrencyId::from_bytes(
            i_address
                .parse::<Address>()
                .expect("a valid i-address")
                .hash(),
        )
    }

    /// The currency from issue #46. Real, and the reason the issue exists: the
    /// daemon prints its `idimportfees` as `1e-8`, and reading its reply as a
    /// registration policy refuses that outright.
    const KAIJU: &str = "iHBwQo7LUmb7QKKqbsd8Kw9BxdQvgTdK9f";

    /// A `getcurrency` reply, trimmed to the fields the SDK reads plus the one
    /// that used to break it. `idimportfees` is `1e-8` verbatim from
    /// `api.verustest.net`, so a definition that parses here is one that really
    /// does come off the wire.
    fn definition(id: &str, name: &str) -> String {
        format!(
            r#"{{"result":{{"currencyid":"{id}","name":"{name}","fullyqualifiedname":"{name}",
               "parent":"{NATIVE}","systemid":"{NATIVE}","startblock":0,"endblock":0,
               "options":33,"proofprotocol":1,"idimportfees":1e-8}},"id":1}}"#
        )
    }

    /// The same reply with its `name` missing: an answer this build cannot read,
    /// standing in for whatever the next unparseable field turns out to be.
    fn unreadable_definition(id: &str) -> String {
        format!(
            r#"{{"result":{{"currencyid":"{id}","fullyqualifiedname":"?","systemid":"{NATIVE}",
               "startblock":0,"endblock":0,"options":33,"proofprotocol":1}},"id":1}}"#
        )
    }

    fn refusal(code: i64, message: &str) -> String {
        format!(r#"{{"error":{{"code":{code},"message":"{message}"}},"id":1}}"#)
    }

    /// A loopback node that answers each request with whichever scripted reply
    /// names a currency the request asked about.
    ///
    /// One reply per connection and `connection: close`, because the whole
    /// point is to answer several *different* requests: `ureq` pools
    /// connections, and a handler that served one and hung up would strand the
    /// next lookup on a dead socket. Plaintext is accepted because loopback is
    /// the one place it is not refused.
    /// A loopback node that answers from a script.
    ///
    /// The accept loop is deliberately not shut down or joined: it owns nothing
    /// but a port and is reaped at process exit. Plumbing a shutdown through it
    /// would mean unblocking `accept` from another thread, which is more moving
    /// parts than the leak costs in a test binary that runs for two seconds.
    fn scripted_node(replies: Vec<(String, String)>) -> Node {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let url = format!("http://{}", listener.local_addr().expect("a bound address"));
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let Some(request) = read_request(&mut stream) else {
                    continue;
                };
                let body = replies
                    .iter()
                    .find(|(asked, _)| request.contains(asked.as_str()))
                    .map(|(_, reply)| reply.clone())
                    .unwrap_or_else(|| refusal(-5, "no currency scripted for that request"));
                let reply = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nconnection: close\r\n\
                     content-length: {}\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(reply.as_bytes());
                let _ = stream.flush();
            }
        });
        node::connect(&Profile {
            name: "stub".into(),
            node: url,
            explorer: String::new(),
            currency: "VRSCTEST".into(),
            allow_spend: false,
            max_response_mb: 8,
            timeout_secs: 5,
        })
        .expect("a client for a loopback url")
    }

    /// A loopback node that accepts a connection and then says nothing at all.
    ///
    /// The failure this module is bounded against. A refused connection fails
    /// fast and costs nothing; a node that takes the request and never answers
    /// is the one that spends the full timeout, and it is what a saturated
    /// public endpoint looks like from here. The accepted streams are parked in
    /// a `Vec` rather than dropped, because closing them would hand the client
    /// an instant transport error instead of the wait being measured.
    fn hanging_node(timeout_secs: u64) -> Node {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let url = format!("http://{}", listener.local_addr().expect("a bound address"));
        std::thread::spawn(move || {
            let mut parked = Vec::new();
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => parked.push(stream),
                    Err(_) => return,
                }
            }
        });
        node::connect(&Profile {
            name: "hanging".into(),
            node: url,
            explorer: String::new(),
            currency: "VRSCTEST".into(),
            allow_spend: false,
            max_response_mb: 8,
            timeout_secs,
        })
        .expect("a client for a loopback url")
    }

    /// Drained until the request is whole rather than after one read: headers
    /// and body can land in separate segments, and answering half a request
    /// reads as a transport failure instead of the reply this is here to send.
    fn read_request(stream: &mut std::net::TcpStream) -> Option<String> {
        let mut request = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let head = match request
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .map(|start| start + 4)
            {
                Some(head) => head,
                None => match stream.read(&mut chunk) {
                    Ok(0) | Err(_) => return None,
                    Ok(read) => {
                        request.extend_from_slice(&chunk[..read]);
                        continue;
                    }
                },
            };
            let declared = String::from_utf8_lossy(&request[..head])
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())?
                })
                .unwrap_or(0);
            if request.len() - head >= declared {
                return Some(String::from_utf8_lossy(&request).into_owned());
            }
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => return None,
                Ok(read) => request.extend_from_slice(&chunk[..read]),
            }
        }
    }

    fn asked_about(ids: &[&str]) -> BTreeSet<CurrencyId> {
        ids.iter().map(|id| currency(id)).collect()
    }

    #[test]
    fn one_currency_the_node_describes_badly_leaves_the_others_named() {
        // The regression this exists for. The lookup used to be one call
        // returning one `Result` for the whole set, so a single currency this
        // build could not read discarded every name already collected — and a
        // wallet holding three tokens printed `(unnamed)` beside all three.
        let node = scripted_node(vec![
            (KAIJU.into(), definition(KAIJU, "Kaiju")),
            (TOKEN.into(), unreadable_definition(TOKEN)),
            (NATIVE.into(), definition(NATIVE, "VRSCTEST")),
        ]);

        let names = look_up_names(
            &node,
            &asked_about(&[KAIJU, TOKEN, NATIVE]),
            Duration::from_secs(30),
        );

        assert!(
            matches!(names.get(&currency(KAIJU)), Some(CurrencyName::Known(name)) if name == "Kaiju"),
            "{:?}",
            names.get(&currency(KAIJU))
        );
        assert!(
            matches!(names.get(&currency(NATIVE)), Some(CurrencyName::Known(name)) if name == "VRSCTEST"),
            "{:?}",
            names.get(&currency(NATIVE))
        );
        // And the one that failed fails alone, with something to say.
        assert!(
            matches!(names.get(&currency(TOKEN)), Some(CurrencyName::Failed(_))),
            "{:?}",
            names.get(&currency(TOKEN))
        );
    }

    #[test]
    fn a_node_that_knows_no_such_currency_is_not_a_lookup_that_failed() {
        // `-8` is what a daemon actually answers a `getcurrency` miss with —
        // measured against api.verustest.net, where `getidentity` answers `-5`
        // and `getcurrency` answers `-8` — and both are accepted, which is this
        // repo's rule at every other place it asks a node whether something
        // exists. Reading only `-5` here would file every currency the chain
        // genuinely does not have under "the lookup failed" instead.
        let node = scripted_node(vec![
            (
                KAIJU.into(),
                refusal(-8, "Invalid currency or currency not found"),
            ),
            (TOKEN.into(), refusal(-1, "internal error")),
        ]);

        let names = look_up_names(
            &node,
            &asked_about(&[KAIJU, TOKEN]),
            Duration::from_secs(30),
        );

        assert!(
            matches!(names.get(&currency(KAIJU)), Some(CurrencyName::Absent)),
            "{:?}",
            names.get(&currency(KAIJU))
        );
        assert!(
            matches!(names.get(&currency(TOKEN)), Some(CurrencyName::Failed(_))),
            "{:?}",
            names.get(&currency(TOKEN))
        );
    }

    #[test]
    fn a_hanging_node_costs_one_timeout_and_not_one_per_currency() {
        // The bound. This is one `getcurrency` per currency and the set is the
        // transaction's to choose, so without a shared deadline five currencies
        // against a node that never answers cost five full timeouts in a row —
        // and at the default 20 seconds a transaction naming ten would sit
        // silent for over three minutes. Timed with a 1-second timeout, where
        // the unbounded version took a hair over five seconds.
        let node = hanging_node(1);
        let wanted = asked_about(&[KAIJU, TOKEN, NATIVE, VETH, BRIDGE]);

        let started = Instant::now();
        let names = look_up_names(&node, &wanted, Duration::from_secs(1));
        let spent = started.elapsed();

        // Generous, because a timeout is not a promise about the microsecond.
        // Five unbounded lookups cannot fit under this; one plus the checks can.
        assert!(
            spent < Duration::from_millis(3_000),
            "the naming step took {spent:?} for {} currencies",
            wanted.len()
        );
        // And it really did stop early rather than getting lucky: the budget
        // was spent by the first request, so the rest were never asked about.
        assert!(names.len() < wanted.len(), "{names:?}");
    }

    #[test]
    fn a_currency_the_budget_never_reached_is_not_a_currency_that_was_asked_about() {
        // The two are different facts and must not collapse into one. A lookup
        // that failed is a question the node was asked and did not answer; a
        // currency the deadline cut off was never asked about at all, and the
        // renderers say different things about them. So no verdict is invented
        // for it — it is simply absent — and `--explain` counts it out loud
        // rather than reporting a shortened run as a complete one.
        let node = hanging_node(1);
        let wanted = asked_about(&[KAIJU, TOKEN, NATIVE, VETH, BRIDGE]);

        let names = look_up_names(&node, &wanted, Duration::from_secs(1));

        let skipped = wanted.len() - names.len();
        assert!(skipped > 0, "nothing was skipped: {names:?}");
        for currency in &wanted {
            assert!(
                !matches!(names.get(currency), Some(CurrencyName::Absent)),
                "a currency nobody asked about was filed as one the node denied"
            );
        }
        let said = name_result(&names, &wanted);
        assert!(
            said.contains(&format!("{skipped} the lookup ran out of time for")),
            "{said}"
        );
    }
}
