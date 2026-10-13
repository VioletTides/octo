//! Song sizes for apps that read a size only to decide whether to show a song.
//!
//! Such an app hides a song whose size is missing or 0, and drops a size above the largest
//! 32-bit number. An outside song has no size until Octo learns the bytes of its stream, so
//! for these apps a finished answer is rewritten: an outside song with no size gets
//! [`PRESENT`], which reads as "this song is here", and any song's size above [`LARGEST`] is
//! held to it. A known size is never changed otherwise, and nothing but `size` is touched.

use octo_core::json::dom::Node;

use crate::xml::XElement;

/// The size an outside song gets while its real size is not known.
pub const PRESENT: i64 = 1;

/// The largest size these apps keep.
pub const LARGEST: i64 = i32::MAX as i64;

/// The element names (XML) and member names (JSON) that hold songs.
const SONG_ROWS: [&str; 3] = ["song", "entry", "child"];

/// The shape of an answer's body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Xml,
    Json,
}

/// The body with its song sizes readable as "here", or None when no song needs a change or
/// the body cannot be read as an answer.
pub fn flag_sizes(body: &[u8], format: Format) -> Option<Vec<u8>> {
    if !may_need_flags(body) {
        return None;
    }
    let text = std::str::from_utf8(body).ok()?;
    match format {
        Format::Xml => {
            let mut root = XElement::parse(text).ok()?;
            flag_xml(&mut root).then(|| root.to_xml_string().into_bytes())
        }
        Format::Json => {
            let mut root = Node::parse(text).ok()?;
            flag_json(&mut root, false).then(|| root.to_json_string(false).into_bytes())
        }
    }
}

/// A cheap look at the bytes: only an answer naming an outside song, or holding a size of ten
/// or more digits, can need a change.
fn may_need_flags(body: &[u8]) -> bool {
    find(body, b"isExternal").is_some()
        || has_long_size(body, b"size=\"")
        || has_long_size(body, b"\"size\":")
}

fn has_long_size(body: &[u8], key: &[u8]) -> bool {
    let mut rest = body;
    while let Some(at) = find(rest, key) {
        rest = &rest[at + key.len()..];
        if rest.iter().take_while(|b| b.is_ascii_digit()).count() >= 10 {
            return true;
        }
    }
    false
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

/// What a song's size says to these apps.
enum Size {
    /// Missing, 0 or below: the song is hidden.
    Absent,
    /// A size they keep as it is.
    Kept,
    /// Above [`LARGEST`]: dropped, so the song is hidden.
    TooLarge,
}

fn read_size(text: Option<&str>) -> Size {
    let Some(text) = text.map(str::trim) else {
        return Size::Absent;
    };
    match text.parse::<i128>() {
        Ok(size) if size > i128::from(LARGEST) => Size::TooLarge,
        Ok(size) if size > 0 => Size::Kept,
        Ok(_) => Size::Absent,
        Err(_) => Size::Kept,
    }
}

/// The new size for a song, if it needs one.
fn new_size(size: Size, outside: bool) -> Option<i64> {
    match size {
        Size::TooLarge => Some(LARGEST),
        Size::Absent if outside => Some(PRESENT),
        Size::Absent | Size::Kept => None,
    }
}

fn flag_xml(element: &mut XElement) -> bool {
    let mut changed = SONG_ROWS.contains(&element.name.as_str()) && flag_xml_song(element);
    for child in element.elements_mut() {
        changed |= flag_xml(child);
    }
    changed
}

fn flag_xml_song(song: &mut XElement) -> bool {
    if song.attribute("isDir") == Some("true") {
        return false;
    }
    let outside = song.attribute("isExternal") == Some("true");
    match new_size(read_size(song.attribute("size")), outside) {
        Some(size) => {
            song.set_attr("size", size.to_string());
            true
        }
        None => false,
    }
}

/// `in_song_rows`: the node is a member named for songs, or an item of one.
fn flag_json(node: &mut Node, in_song_rows: bool) -> bool {
    match node {
        Node::Object(members) => {
            let mut changed = in_song_rows && flag_json_song(members);
            for (name, member) in members.iter_mut() {
                changed |= flag_json(member, SONG_ROWS.contains(&name.as_str()));
            }
            changed
        }
        Node::Array(items) => {
            let mut changed = false;
            for item in items.iter_mut() {
                changed |= flag_json(item, in_song_rows);
            }
            changed
        }
        _ => false,
    }
}

fn flag_json_song(song: &mut indexmap::IndexMap<String, Node>) -> bool {
    if matches!(song.get("isDir"), Some(Node::Bool(true))) {
        return false;
    }
    let outside = matches!(song.get("isExternal"), Some(Node::Bool(true)));
    let size = match song.get("size") {
        None | Some(Node::Null) => Size::Absent,
        Some(Node::Number(text)) => read_size(Some(text)),
        Some(_) => Size::Kept,
    };
    match new_size(size, outside) {
        Some(size) => {
            song.insert("size".to_string(), Node::Number(size.to_string()));
            true
        }
        None => false,
    }
}

#[cfg(test)]
#[path = "size_flag_tests.rs"]
mod tests;
