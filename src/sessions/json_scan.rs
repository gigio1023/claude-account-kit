//! A top-level key scanner for JSON objects that are too large to decode.
//!
//! A transcript line holds a whole message, and reading only metadata must
//! never decode it: the content can be huge, and it may be cut off at the end of
//! a read window. The scanner walks the outer object by bracket depth, so a
//! value is kept as a byte slice and only short, approved scalar values reach
//! `serde_json`. Keys of nested objects are never seen.

use serde_json::{Map, Value};

/// Keys whose value marks a remote transport; only their presence is kept.
const REMOTE_KEYS: [&str; 2] = ["sshConfig", "wslConfig"];
/// Scalar values longer than this stay undecoded.
const MAX_SCALAR_BYTES: usize = 8192;
/// A key longer than this ends the scan; real metadata keys are short.
const MAX_KEY_BYTES: usize = 128;

/// Decode the approved scalar fields of one JSON object.
///
/// Objects and arrays are skipped, `sshConfig` and `wslConfig` become a
/// boolean that says whether they hold anything, and the scan stops at the
/// first malformed or truncated pair, keeping what came before it.
pub(super) fn scalar_fields(raw: &[u8], keys: &[&str]) -> Map<String, Value> {
    let mut result = Map::new();
    for (key, value) in TopLevelValues::new(raw) {
        if REMOTE_KEYS.contains(&key.as_str()) {
            let present = !matches!(value, b"null" | b"false" | b"{}" | b"[]" | b"\"\"");
            result.insert(key, Value::Bool(present));
        } else if keys.contains(&key.as_str())
            && value.len() <= MAX_SCALAR_BYTES
            && !matches!(value.first(), Some(b'{' | b'['))
            && let Ok(decoded) = serde_json::from_slice::<Value>(value)
        {
            result.insert(key, decoded);
        }
    }
    result
}

/// The raw bytes of one top-level value, such as the `entries` array of an index.
pub(super) fn raw_field<'a>(raw: &'a [u8], key: &str) -> Option<&'a [u8]> {
    TopLevelValues::new(raw)
        .filter(|(name, _)| name == key)
        .last()
        .map(|(_, value)| value)
}

/// The raw bytes of each item of a JSON array, stopping at the first malformed one.
pub(super) fn array_items(raw: &[u8]) -> Vec<&[u8]> {
    let mut items = Vec::new();
    if raw.first() != Some(&b'[') {
        return items;
    }
    let mut position = 1;
    while position < raw.len() {
        position = skip(raw, position, b", \t\r\n");
        match value_end(raw, position) {
            Some(end) if end > position => {
                items.push(&raw[position..end]);
                position = end;
            }
            _ => break,
        }
    }
    items
}

/// Iterator over the `"key": value` pairs of the outermost object.
struct TopLevelValues<'a> {
    raw: &'a [u8],
    position: usize,
    done: bool,
}

impl<'a> TopLevelValues<'a> {
    fn new(raw: &'a [u8]) -> Self {
        let position = skip(raw, 0, b" \t\r\n");
        let opens_object = raw.get(position) == Some(&b'{');
        Self {
            raw,
            position: position + 1,
            done: !opens_object,
        }
    }

    fn next_pair(&mut self) -> Option<(String, &'a [u8])> {
        let raw = self.raw;
        let start = skip(raw, self.position, b" \t\r\n,");
        if raw.get(start) != Some(&b'"') {
            return None;
        }
        let key_end = value_end(raw, start)?;
        if key_end - start > MAX_KEY_BYTES {
            return None;
        }
        let key: String = serde_json::from_slice(&raw[start..key_end]).ok()?;
        let colon = skip(raw, key_end, b" \t\r\n");
        if raw.get(colon) != Some(&b':') {
            return None;
        }
        let value_start = skip(raw, colon + 1, b" \t\r\n");
        let end = value_end(raw, value_start)?;
        self.position = end;
        Some((key, &raw[value_start..end]))
    }
}

impl<'a> Iterator for TopLevelValues<'a> {
    type Item = (String, &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let pair = self.next_pair();
        if pair.is_none() {
            self.done = true;
        }
        pair
    }
}

fn skip(raw: &[u8], mut position: usize, allowed: &[u8]) -> usize {
    while position < raw.len() && allowed.contains(&raw[position]) {
        position += 1;
    }
    position
}

/// The end of the JSON value starting at `start`, by string and bracket depth.
///
/// A string ends at its closing quote, a container at its matching bracket,
/// and a bare scalar at the first separator. `None` means the value is cut off.
fn value_end(raw: &[u8], start: usize) -> Option<usize> {
    let mut position = start;
    let mut depth = 0_usize;
    while position < raw.len() {
        match raw[position] {
            b'"' => {
                let string_end = string_end(raw, position);
                if depth == 0 || string_end.is_none() {
                    return string_end;
                }
                position = string_end?;
                continue;
            }
            b'{' | b'[' => depth += 1,
            b'}' | b']' => {
                if depth == 0 {
                    return Some(position);
                }
                depth -= 1;
                if depth == 0 {
                    return Some(position + 1);
                }
            }
            b',' | b' ' | b'\t' | b'\r' | b'\n' if depth == 0 => return Some(position),
            _ => {}
        }
        position += 1;
    }
    (depth == 0 && position > start).then_some(position)
}

fn string_end(raw: &[u8], start: usize) -> Option<usize> {
    let mut position = start + 1;
    while position < raw.len() {
        match raw[position] {
            b'\\' => position += 2,
            b'"' => return Some(position + 1),
            _ => position += 1,
        }
    }
    None
}
