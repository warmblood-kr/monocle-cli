//! Output redaction shared by the `craft turn` lanes.

use std::io::Write;

use serde_json::{json, Value};

/// The ONE output boundary: everything the command writes to stdout/stderr goes
/// through here, so a server echoing a secret (the access token) never reaches a
/// terminal or a pipe. Empty secrets are ignored.
///
/// Threat model: ACCIDENTAL reflection only (a server/proxy echoing headers or
/// tokens in errors/messages, logs, CI capture). A malicious Craft server already
/// holds the token; adversarial splitting is out of scope. So: a secret split
/// across deltas is caught by the hold-back; a split across a delta and a later
/// non-delta text is caught by `partial_redacted` (the held-back tail ending in a
/// secret prefix is redacted at flush, and the next text loses a leading run that
/// is a secret suffix of >= 8 chars). Accepted false positive: a tail that merely
/// ends in the secret's first char is redacted too.
pub struct Redactor {
    forms: Vec<String>,
    /// Hold-back tail of the assistant delta stream (see [`Redactor::delta`]).
    hold: std::cell::RefCell<String>,
    /// A flush redacted a possible secret prefix; the next text may start with its rest.
    partial_redacted: std::cell::Cell<bool>,
}

impl Redactor {
    pub fn new(secrets: &[&str]) -> Self {
        let mut r = Self {
            forms: Vec::new(),
            hold: Default::default(),
            partial_redacted: Default::default(),
        };
        secrets.iter().for_each(|s| r.add_secret(s));
        r
    }

    /// Start redacting one more secret (call before any text is streamed).
    pub fn add_secret(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        self.forms.push(s.to_string());
        // The form it takes inside a serialized JSON string.
        let esc = serde_json::to_string(s).unwrap_or_default();
        let esc = esc.trim_matches('"');
        if esc != s {
            self.forms.push(esc.to_string());
        }
    }

    pub fn scrub(&self, t: &str) -> String {
        self.forms
            .iter()
            .fold(t.to_string(), |t, s| t.replace(s, "[REDACTED]"))
    }

    /// Feed one assistant delta through the single scrubbed stream. A secret split
    /// across deltas can't leak: the last `longest form - 1` chars stay unemitted
    /// until more text (or [`Redactor::flush_stream`]) arrives. Returns what is safe to emit.
    pub fn delta(&self, t: &str) -> String {
        let mut b = self.hold.borrow_mut();
        b.push_str(t);
        let s = self.scrub(&b);
        let keep = self
            .forms
            .iter()
            .map(|f| f.chars().count())
            .max()
            .unwrap_or(0)
            .saturating_sub(1);
        let cut = s.chars().count().saturating_sub(keep);
        let at = s.char_indices().nth(cut).map_or(s.len(), |(i, _)| i);
        *b = s[at..].to_string();
        if at > 0 {
            self.partial_redacted.set(false); // unrelated text went out
        }
        s[..at].to_string()
    }

    /// Take the held-back tail (scrubbed; a trailing possible secret prefix is
    /// redacted). Empty when nothing was held.
    pub fn take_tail(&self) -> String {
        let mut tail = self.scrub(&std::mem::take(&mut *self.hold.borrow_mut()));
        // Longest suffix of the tail that is a prefix of a secret form (char boundary).
        let cut = tail
            .char_indices()
            .map(|(i, _)| i)
            .find(|&i| self.forms.iter().any(|f| f.starts_with(&tail[i..])));
        if let Some(i) = cut {
            tail.truncate(i);
            tail.push_str("[REDACTED]");
            self.partial_redacted.set(true);
        }
        tail
    }

    /// Emit the held-back tail (scrubbed). Text mode: raw; `--json`: a synthetic delta frame.
    pub fn flush_stream(&self, json: bool) -> bool {
        let tail = self.take_tail();
        if tail.is_empty() {
            return false;
        }
        if json {
            let f = json!({"type": "assistant_delta", "data": {"text": tail}});
            self.emit("ws", "frame", &f);
        } else {
            self.out(&tail);
        }
        true
    }

    /// Non-delta text: if a partial secret was redacted, also drop a leading run of
    /// `t` that is a secret suffix (>= 8 chars, longest match). Clears the flag.
    pub fn lead(&self, t: &str) -> String {
        if !self.partial_redacted.replace(false) {
            return t.to_string();
        }
        let n = self
            .forms
            .iter()
            .flat_map(|f| f.char_indices().map(move |(i, _)| &f[i..]))
            .filter(|s| s.chars().count() >= 8 && t.starts_with(s))
            .map(str::len)
            .max();
        n.map_or(t.to_string(), |n| format!("[REDACTED]{}", &t[n..]))
    }

    /// `--json`: [`Redactor::lead`] on the text-ish string fields of an object.
    pub fn lead_json(&self, v: &mut Value) {
        for k in ["text", "message", "content", "error"] {
            if let Some(t) = v[k].as_str() {
                v[k] = json!(self.lead(t));
            }
        }
        self.partial_redacted.set(false);
    }

    pub fn out(&self, t: &str) {
        print!("{}", self.scrub(t));
        let _ = std::io::stdout().flush();
    }

    pub fn errln(&self, t: &str) {
        eprintln!("{}", self.scrub(t));
    }

    /// `--json` line: serialize, scrub the string, print.
    pub fn emit(&self, source: &str, key: &str, v: &Value) {
        self.out(&format!("{}\n", json!({"source": source, key: v})));
    }
}
