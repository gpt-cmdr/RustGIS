//! Keeping a long conversation within the model's context: the agent sends what it has, and when
//! that grows past [`BUDGET`] (or the provider says it's too long) it compacts the finished part of
//! the conversation, everything before the user's latest message, so a run under way is never
//! cut. Large tool results and old reasoning are trimmed first; if that isn't enough, the model
//! summarises those turns and the summary replaces them. Calls stay with their results.
//!
//! Plain functions over [`Turn`]s, without I/O: the agent sends the summary request like any other.

use serde_json::Value;

use crate::provider::{Api, Turn};

/// The conversation's size (in characters, about four to a token) past which it is compacted:
/// about 100,000 tokens, within any current model's context with room for the reply.
pub const BUDGET: usize = 400_000;

/// Older tool results are cut to this many characters when trimming.
const TRIMMED_RESULT: usize = 1_500;

/// The instructions of the summary request.
pub const SUMMARY_PROMPT: &str = "You summarise an earlier part of a conversation between a user and the AI assistant of \
RustGIS, a GIS, so the conversation can go on without it. Write a concise summary for the assistant to read: what the user \
asked for, what was done to the map (layers added or changed, by name; commands and tools run and their results), what was \
found in the data, decisions and preferences the user stated, and anything left to do. Keep names, numbers and ids exact. \
Write only the summary.";

/// About how many characters the turns take in a request.
pub fn size(turns: &[Turn]) -> usize {
    turns
        .iter()
        .map(|t| match t {
            Turn::User(text) => text.len(),
            Turn::Assistant { text, calls, native } => {
                text.len()
                    + calls.iter().map(|c| c.name.len() + c.args.to_string().len()).sum::<usize>()
                    + native.as_ref().map_or(0, |(_, v)| v.to_string().len())
            }
            Turn::Results(rs) => rs.iter().map(|r| r.content.len()).sum(),
        })
        .sum()
}

/// Where the finished part of the conversation ends: at the user's latest message (0 when there
/// is nothing before it).
pub fn boundary(turns: &[Turn]) -> usize {
    turns.iter().rposition(|t| matches!(t, Turn::User(_))).unwrap_or(0)
}

/// Trims the turns before `upto`: long tool results are cut, and reasoning the model kept from
/// earlier replies (Responses reasoning items, Claude's thinking blocks) is dropped; providers
/// accept earlier turns without it. Says whether anything changed.
pub fn trim(turns: &mut [Turn], upto: usize) -> bool {
    let mut changed = false;
    for t in turns.iter_mut().take(upto) {
        match t {
            Turn::Results(rs) => {
                for r in rs.iter_mut() {
                    if r.content.len() > TRIMMED_RESULT {
                        r.content = format!("{} … [trimmed]", r.content.chars().take(TRIMMED_RESULT).collect::<String>());
                        changed = true;
                    }
                }
            }
            Turn::Assistant { native: Some((api, v)), .. } => {
                let thinking: &[&str] = match api {
                    Api::Responses => &["reasoning"],
                    Api::Anthropic => &["thinking", "redacted_thinking"],
                    _ => &[],
                };
                if let Some(items) = v.as_array_mut() {
                    let before = items.len();
                    items.retain(|i| !thinking.contains(&i.get("type").and_then(Value::as_str).unwrap_or("")));
                    changed |= items.len() != before;
                }
            }
            _ => {}
        }
    }
    changed
}

/// The turns before `upto` as text, for the summary request.
pub fn as_text(turns: &[Turn], upto: usize) -> String {
    let clip = |s: &str, n: usize| -> String {
        if s.chars().count() > n { format!("{} …", s.chars().take(n).collect::<String>()) } else { s.to_string() }
    };
    let mut out = String::new();
    for t in turns.iter().take(upto) {
        match t {
            Turn::User(text) => out.push_str(&format!("User: {}\n\n", clip(text, 4_000))),
            Turn::Assistant { text, calls, .. } => {
                if !text.trim().is_empty() {
                    out.push_str(&format!("Assistant: {}\n\n", clip(text, 4_000)));
                }
                for c in calls {
                    out.push_str(&format!("Assistant called {} {}\n", c.name, clip(&c.args.to_string(), 600)));
                }
            }
            Turn::Results(rs) => {
                for r in rs {
                    let mark = if r.error { "failed" } else { "returned" };
                    out.push_str(&format!("{} {mark}: {}\n", r.name, clip(&r.content, 800)));
                }
                out.push('\n');
            }
        }
    }
    out
}

/// Replaces the turns before `upto` with `summary`, which goes at the start of the user's message
/// that follows them (two user turns in a row are refused by some providers).
pub fn apply(turns: &mut Vec<Turn>, upto: usize, summary: &str) {
    if upto == 0 || upto > turns.len() {
        return;
    }
    turns.drain(..upto);
    let block = format!("<summary of the earlier conversation>\n{}\n</summary of the earlier conversation>\n\n", summary.trim());
    match turns.first_mut() {
        Some(Turn::User(text)) => text.insert_str(0, &block),
        _ => turns.insert(0, Turn::User(block)),
    }
}

/// Whether an error says the conversation is too long for the model.
pub fn overflowed(e: &crate::Error) -> bool {
    let text = format!("{} {}", e.code.as_deref().unwrap_or(""), e.detail).to_lowercase();
    [
        "context_length_exceeded",
        "context length",
        "context window",
        "prompt is too long",
        "too many tokens",
        "input is too long",
        "maximum number of tokens",
        "exceeds the maximum",
    ]
    .iter()
    .any(|p| text.contains(p))
}
