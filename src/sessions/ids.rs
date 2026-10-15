//! The two identifiers that name one conversation.
//!
//! Claude Code names a transcript by a session UUID, and Desktop names its
//! list entry `local_<uuid>`. The two start out equal for a session the CLI
//! began, but Desktop's rewind and fork give an entry a new transcript id while
//! the entry keeps its name, so they are separate types.

use serde_json::Value;
use std::fmt;

/// A canonical, lowercase, hyphenated UUID.
///
/// It is the only form Claude Code and Desktop write. Anything else, including
/// an uppercase or braced UUID, is rejected so that an identifier can never
/// carry path or file name syntax into a path this tool builds.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct SessionId(String);

impl SessionId {
    pub(super) fn parse(value: &str) -> Option<Self> {
        let bytes = value.as_bytes();
        if bytes.len() != 36 {
            return None;
        }
        let canonical = bytes.iter().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => *byte == b'-',
            _ => byte.is_ascii_digit() || (b'a'..=b'f').contains(byte),
        });
        canonical.then(|| Self(value.to_string()))
    }

    /// Read an identifier from a JSON value, treating any other type as absent.
    pub(super) fn from_value(value: Option<&Value>) -> Option<Self> {
        value.and_then(Value::as_str).and_then(Self::parse)
    }

    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// The `local_<uuid>` name of a Desktop list entry.
///
/// It is the wrapper's file stem and the `sessionId` inside it, and it stays
/// the same in every account and organization that lists the conversation.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct WrapperId(SessionId);

impl WrapperId {
    /// Parse a file stem such as `local_<uuid>`.
    pub(super) fn parse_stem(stem: &str) -> Option<Self> {
        stem.strip_prefix("local_")
            .and_then(SessionId::parse)
            .map(Self)
    }

    /// The entry name this tool gives a session that only the CLI knows.
    pub(super) fn for_cli_session(id: &SessionId) -> Self {
        Self(id.clone())
    }

    pub(super) fn uuid(&self) -> &SessionId {
        &self.0
    }

    pub(super) fn file_name(&self) -> String {
        format!("local_{}.json", self.0)
    }
}

impl fmt::Display for WrapperId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "local_{}", self.0)
    }
}
