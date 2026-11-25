//! Which title every copy of a conversation should show.
//!
//! Each Desktop app keeps its own title for a conversation, and a rename in
//! one app reaches no other. Desktop records who set a title in
//! `titleSource`: `user` for a rename, `tool` for an agent, and `auto` for a
//! generated title, which Desktop never lets replace the other two. It also
//! appends the replaced title to `previousTitles` on every change.
//!
//! The transcript's `custom-title` records cannot order renames. Desktop
//! appends the title of whichever app runs a turn, so after a rename in one
//! app the next turn in another app appends that app's older title again.
//!
//! - A name set in Desktop: the `user` or `tool` title of any listed or
//!   archived copy. When copies hold different such names, the one whose copy
//!   lists all the others in `previousTitles`, and is listed by none of them,
//!   came last. Otherwise the conversation is left alone as a conflict.
//! - A name set outside Desktop: with no such name in any copy, the last
//!   `custom-title` in the transcript's tail, when no copy holds it and no
//!   copy's title appears in the tail at all. Desktop would have appended its
//!   own title with any turn it ran, so the name came from the terminal,
//!   through `claude --name` or `/rename`. Without a `custom-title` record in
//!   the tail, titles stay as they are; the whole transcript is never read.
//! - Otherwise every copy keeps its title.

use super::desktop::WrapperCopy;
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;

/// The title a conversation's copies should show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TitleTarget {
    pub(super) title: String,
    /// The `titleSource` to give each retitled copy: the source of the name
    /// set in Desktop, so Desktop keeps it over a generated title, or none for
    /// a name set in the terminal, so a later terminal rename is still
    /// recognized.
    pub(super) source: Option<String>,
}

/// What the copies and the transcript tail say about the title.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum TitleDecision {
    Keep,
    Set(TitleTarget),
    /// Copies hold different names set in Desktop, and none of them is
    /// provably the latest.
    Conflict,
}

/// Decide the title from every copy of one conversation and the
/// `custom-title` values of its current transcript's tail, oldest first.
pub(super) fn decide(copies: &[&WrapperCopy], tail_titles: &[String]) -> TitleDecision {
    let renamed: Vec<&WrapperCopy> = copies
        .iter()
        .copied()
        .filter(|copy| !copy.fields.hidden && deliberate(copy))
        .collect();
    let candidates: BTreeSet<&str> = renamed
        .iter()
        .filter_map(|copy| copy.fields.title.as_deref())
        .collect();
    if !candidates.is_empty() {
        return match latest_name(&renamed, &candidates) {
            Some(name) => TitleDecision::Set(TitleTarget {
                title: name.to_string(),
                source: renamed
                    .iter()
                    .find(|copy| copy.fields.title.as_deref() == Some(name))
                    .and_then(|copy| copy.fields.title_source.clone()),
            }),
            None => TitleDecision::Conflict,
        };
    }
    let Some(last) = tail_titles.last() else {
        return TitleDecision::Keep;
    };
    let held = |title: &str| {
        copies
            .iter()
            .any(|copy| copy.fields.title.as_deref() == Some(title))
    };
    if tail_titles.iter().any(|title| held(title)) {
        return TitleDecision::Keep;
    }
    TitleDecision::Set(TitleTarget {
        title: last.clone(),
        source: None,
    })
}

/// Whether a copy's title was set by a person or an agent rather than generated.
fn deliberate(copy: &WrapperCopy) -> bool {
    copy.fields.title.is_some()
        && matches!(copy.fields.title_source.as_deref(), Some("user" | "tool"))
}

/// The one name whose copies list every other name as replaced, while no
/// copy of another name lists it.
fn latest_name<'a>(renamed: &[&WrapperCopy], candidates: &BTreeSet<&'a str>) -> Option<&'a str> {
    let replaced = |name: &str, other: &str| {
        renamed.iter().any(|copy| {
            copy.fields.title.as_deref() == Some(name)
                && copy
                    .fields
                    .previous_titles
                    .iter()
                    .any(|title| title == other)
        })
    };
    let mut latest = candidates.iter().copied().filter(|name| {
        candidates
            .iter()
            .filter(|other| *other != name)
            .all(|other| replaced(name, other) && !replaced(other, name))
    });
    let first = latest.next()?;
    latest.next().is_none().then_some(first)
}

/// Whether `payload` shows another title. A copy that already shows the
/// target keeps its `titleSource`, so a run changes no copy for that alone.
pub(super) fn differs(payload: &Map<String, Value>, target: &TitleTarget) -> bool {
    payload.get("title").and_then(Value::as_str) != Some(target.title.as_str())
}

/// Set the title, and the title source when the target has one.
pub(super) fn apply(payload: &mut Map<String, Value>, target: &TitleTarget) {
    payload.insert("title".to_string(), json!(target.title));
    if let Some(source) = &target.source {
        payload.insert("titleSource".to_string(), json!(source));
    }
}
