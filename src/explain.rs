//! `--explain`: the SDK calls a command actually made.
//!
//! This is the feature the whole program is an argument for. Reading what
//! `pecu send` did should teach you how to do it yourself, so every call into
//! `verus-sdk` is recorded at the call site with the arguments it was given and
//! a one-line summary of what came back.
//!
//! # Why this is not a tracing layer
//!
//! The obvious design is `tracing` with a subscriber that prints spans. It does
//! not work here: `verus-sdk` emits no spans, so the events would have to be
//! written at the call site anyway — and then the subscriber is pure ceremony
//! between a `debug!` and a `println!`. Recording explicitly keeps the recorded
//! text next to the call it describes, where it can be kept honest.
//!
//! The cost is that this is only as accurate as the call sites keep it. A call
//! added without a matching [`Explain::call`] is invisible.

use std::cell::RefCell;

use crate::ui::{Panel, Text, Theme};

struct Call {
    call: String,
    result: Option<String>,
}

/// Records SDK calls when `--explain` is on, and nothing at all when it is not.
pub struct Explain {
    enabled: bool,
    calls: RefCell<Vec<Call>>,
}

impl Explain {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            calls: RefCell::new(Vec::new()),
        }
    }

    /// Record a call about to be made. Written as the Rust that would make it.
    pub fn call(&self, call: impl Into<String>) {
        if !self.enabled {
            return;
        }
        self.calls.borrow_mut().push(Call {
            call: call.into(),
            result: None,
        });
    }

    /// Summarise what the most recent call returned.
    pub fn result(&self, result: impl Into<String>) {
        if !self.enabled {
            return;
        }
        if let Some(last) = self.calls.borrow_mut().last_mut() {
            last.result = Some(result.into());
        }
    }

    /// The panel, or `None` when the flag is off or nothing was recorded.
    pub fn panel(&self, theme: &Theme) -> Option<Panel> {
        if !self.enabled {
            return None;
        }
        let calls = self.calls.borrow();
        if calls.is_empty() {
            return None;
        }

        let palette = theme.palette;
        let glyphs = theme.glyphs;
        let mut panel = Panel::new("SDK CALLS");
        for (index, call) in calls.iter().enumerate() {
            if index > 0 {
                panel = panel.blank();
            }
            panel = panel.wrapped(0, Text::of(&call.call, palette.value));
            if let Some(result) = &call.result {
                panel = panel.wrapped(
                    2,
                    Text::of(glyphs.arrow, palette.muted)
                        .space()
                        .push(result, palette.accent),
                );
            }
        }
        Some(panel.note(Text::of(
            "every line above is a real call this command made",
            palette.muted,
        )))
    }
}
