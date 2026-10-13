//! Reading a JSON request body into the shape an action expects, and the 400 validation problem
//! answered when it cannot be read.
//!
//! A body is read in one pass, token by token, so the first complaint is the first thing wrong
//! in reading order: a broken payload (the reader's words) or a member of the wrong type (`The
//! JSON value could not be converted to ...`), both with the path reached (`$.scope`,
//! `$.keys[1]`) and where the reader stood. [`Shape`] says what each action's body holds, so
//! every endpoint answers for its own body.

use axum::http::{HeaderMap, StatusCode, header};
use axum::response::Response;
use octo_core::json::dom::Node;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::http::error::{problem, validation_problem};

// ---- the body shapes --------------------------------------------------------------------------

/// A type a body is read into: its full name as the error messages spell it (`Octo.Controllers.
/// ImportsController+OnRequest`), whether it is filled through its constructor (a positional
/// record) or its setters (a class), and its members.
#[derive(Debug)]
pub struct Shape {
    pub name: &'static str,
    pub by_constructor: bool,
    pub members: &'static [Member],
}

/// One member of a [`Shape`]. Names are matched ignoring case.
#[derive(Debug)]
pub struct Member {
    pub name: &'static str,
    pub kind: Kind,
}

/// What a member holds, as far as reading it can fail.
#[derive(Debug)]
pub enum Kind {
    /// A boolean.
    Bool,
    /// A 32-bit integer (also read from a string of digits).
    Int32,
    /// A 64-bit integer (also read from a string of digits).
    Int64,
    /// A 32-bit integer or null.
    OptionalInt32,
    /// A string, or null.
    Text,
    /// A list of strings, or null.
    TextList,
    /// A map of strings to strings, or null.
    TextMap,
    /// Another record or class.
    Record(&'static Shape),
    /// A list of another record or class, or null.
    RecordList(&'static Shape),
}

impl Kind {
    /// The type's name in the serializer's message.
    fn clr_name(&self) -> String {
        match self {
            Kind::Bool => "System.Boolean".into(),
            Kind::Int32 => "System.Int32".into(),
            Kind::Int64 => "System.Int64".into(),
            Kind::OptionalInt32 => "System.Nullable`1[System.Int32]".into(),
            Kind::Text => "System.String".into(),
            Kind::TextList => "System.Collections.Generic.List`1[System.String]".into(),
            Kind::TextMap => "System.Collections.Generic.Dictionary`2[System.String,System.String]".into(),
            Kind::Record(shape) => shape.name.into(),
            Kind::RecordList(shape) => format!("System.Collections.Generic.List`1[{}]", shape.name),
        }
    }
}

/// A request body type and the shape it is read as.
pub trait FromBody: DeserializeOwned {
    const SHAPE: &'static Shape;
}

/// A positional record of string members only, the common case:
/// `text_record!("Octo.Controllers.X+Y", "a", "b")`.
macro_rules! text_record {
    ($name:expr $(, $member:expr)* $(,)?) => {
        &$crate::controllers::admin::json_body::Shape {
            name: $name,
            by_constructor: true,
            members: &[$($crate::controllers::admin::json_body::Member {
                name: $member,
                kind: $crate::controllers::admin::json_body::Kind::Text,
            }),*],
        }
    };
}
pub(crate) use text_record;

// ---- binding -----------------------------------------------------------------------------------

pub(crate) fn is_json_content_type(headers: &HeaderMap) -> bool {
    let Some(value) = headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let media = value.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    media == "application/json"
        || media == "text/json"
        || (media.starts_with("application/") && media.ends_with("+json"))
}

/// A required JSON body named `name` (case-insensitive member names, numbers from strings), with
/// the automatic answers given before the action runs. A body that is not JSON by its content
/// type is a 415; an empty one, `null` or one that cannot be read into `T`'s shape is the 400
/// validation problem.
#[allow(clippy::result_large_err)]
pub fn bind_body<T: FromBody>(headers: &HeaderMap, body: &[u8], name: &str) -> Result<T, Response> {
    bind_body_as(headers, body, name, T::SHAPE)
}

/// [`bind_body`] with the record named here, for a body read as something else than its own
/// record type (a `Value`).
#[allow(clippy::result_large_err)]
pub fn bind_body_as<T: DeserializeOwned>(
    headers: &HeaderMap,
    body: &[u8],
    name: &str,
    shape: &Shape,
) -> Result<T, Response> {
    if !is_json_content_type(headers) {
        return Err(problem(StatusCode::UNSUPPORTED_MEDIA_TYPE));
    }
    let required = format!("The {name} field is required.");
    if body.is_empty() {
        return Err(validation_problem(&[
            ("", &["A non-empty request body is required."]),
            (name, &[required.as_str()]),
        ]));
    }
    match read(body, shape) {
        Ok(None) => Err(validation_problem(&[(name, &[required.as_str()])])),
        Ok(Some(value)) => Ok(value),
        // The errors are listed by key: the parameter's own before the reader's `$...`.
        Err((path, message)) => Err(validation_problem(&[
            (name, &[required.as_str()]),
            (path.as_str(), &[message.as_str()]),
        ])),
    }
}

/// An optional JSON body: an empty body or `null` reads as None instead of the 400, and a body
/// that cannot be read is the 400 with the reader's entry alone (no "field is required", since
/// the body is not).
#[allow(clippy::result_large_err)]
pub fn bind_optional_body<T: FromBody>(headers: &HeaderMap, body: &[u8]) -> Result<Option<T>, Response> {
    if body.is_empty() {
        return Ok(None);
    }
    if !is_json_content_type(headers) {
        return Err(problem(StatusCode::UNSUPPORTED_MEDIA_TYPE));
    }
    read(body, T::SHAPE)
        .map_err(|(path, message)| validation_problem(&[(path.as_str(), &[message.as_str()])]))
}

/// Reads a non-empty body into `T`, checked against `shape`: None for `null`, or the error's key
/// and message.
fn read<T: DeserializeOwned>(body: &[u8], shape: &Shape) -> Result<Option<T>, (String, String)> {
    let text = String::from_utf8_lossy(body);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    if let Some(error) = read_error(text, shape) {
        let message = format!("{} Path: {} | {}", error.message, error.path, error.position());
        return Err((error.path, message));
    }
    let value: Value = serde_json::from_str(text).map_err(|e| ("$".to_string(), e.to_string()))?;
    if value.is_null() {
        return Ok(None);
    }
    octo_core::json::web::from_value(&value).map(Some).map_err(|e| {
        // The shape check above passed, so the Rust type is stricter than its shape.
        tracing::warn!("A request body the C# record took did not fit its Rust type: {e}");
        (
            "$".to_string(),
            format!(
                "The JSON value could not be converted to {}. Path: $ | LineNumber: 0 | BytePositionInLine: {}.",
                shape.name,
                text.len()
            ),
        )
    })
}

/// The body of an action that reads the raw body itself: UTF-8 with a BOM dropped.
pub fn body_text(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    text.strip_prefix('\u{feff}').unwrap_or(&text).to_string()
}

/// Parses `text` as a [`Node`] tree with the reader's default options (no comments, no trailing
/// commas): the tree, or the reader's message.
pub fn parse_json_node(text: &str) -> Result<Node, String> {
    if let Some(error) = stj_parse_error(text) {
        return Err(format!("{} {}", error.message, error.position()));
    }
    Node::parse(text)
}

// ---- the reader ------------------------------------------------------------------------------

/// Where and why a payload was refused, in the API's words: the reader's (broken JSON) or the
/// serializer's (a member it could not convert).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StjReadError {
    pub message: String,
    /// The serializer's path when it gave up (`$` for a plain read).
    pub path: String,
    pub line: usize,
    pub byte_in_line: usize,
}

impl StjReadError {
    pub fn position(&self) -> String {
        format!(
            "LineNumber: {} | BytePositionInLine: {}.",
            self.line, self.byte_in_line
        )
    }
}

/// Reads `text` with the reader's default options (one value, no comments, no trailing commas,
/// depth at most 64) and returns its first complaint, or None when it is valid JSON. The common
/// refusals carry the reference messages word for word; the rarer ones a close paraphrase.
pub fn stj_parse_error(text: &str) -> Option<StjReadError> {
    let mut reader = StjReader::new(text);
    reader.document(None).err()
}

/// Reads `text` into `shape` and returns its first complaint, or None when it reads (`null`
/// included).
pub fn read_error(text: &str, shape: &Shape) -> Option<StjReadError> {
    let mut reader = StjReader::new(text);
    reader.document(Some(shape)).err()
}

const MAX_DEPTH: usize = 64;

const ZERO_DEPTH_AT_END: &str = "Expected depth to be zero at the end of the JSON payload. There is an open JSON object or array that should be closed.";
const NOTHING_AFTER_COMMA: &str =
    "Expected start of a property name or value, but instead reached end of data.";

fn printable(b: u8) -> String {
    if (0x20..0x7f).contains(&b) {
        (b as char).to_string()
    } else {
        format!("0x{b:02X}")
    }
}

/// A step of the serializer's path: a member or dictionary key, or a list's element.
enum Step {
    Name(String),
    Index(usize),
}

/// A name as a path step: `.name`, or `['name']` when it holds a character a path uses.
fn push_name(path: &mut String, name: &str) {
    const SPECIAL: &[char] = &[
        '.', ' ', '\'', '/', '"', '[', ']', '(', ')', '\t', '\n', '\r', '\u{c}', '\u{8}', '\\', '\u{85}',
        '\u{2028}', '\u{2029}',
    ];
    if name.contains(SPECIAL) {
        path.push_str("['");
        path.push_str(name);
        path.push_str("']");
    } else {
        path.push('.');
        path.push_str(name);
    }
}

struct StjReader<'a> {
    s: &'a [u8],
    i: usize,
    line: usize,
    line_start: usize,
    path: Vec<Step>,
}

/// Where the reader stood, for an error reported after reading on.
#[derive(Clone, Copy)]
struct Mark {
    i: usize,
    line: usize,
    line_start: usize,
}

impl<'a> StjReader<'a> {
    fn new(text: &'a str) -> Self {
        StjReader {
            s: text.as_bytes(),
            i: 0,
            line: 0,
            line_start: 0,
            path: Vec::new(),
        }
    }

    fn path_text(&self) -> String {
        let mut path = "$".to_string();
        for step in &self.path {
            match step {
                Step::Name(name) => push_name(&mut path, name),
                Step::Index(index) => path.push_str(&format!("[{index}]")),
            }
        }
        path
    }

    fn mark(&self) -> Mark {
        Mark {
            i: self.i,
            line: self.line,
            line_start: self.line_start,
        }
    }

    fn fail(&self, message: impl Into<String>) -> StjReadError {
        self.fail_at(self.mark(), message)
    }

    fn fail_at(&self, at: Mark, message: impl Into<String>) -> StjReadError {
        StjReadError {
            message: message.into(),
            path: self.path_text(),
            line: at.line,
            byte_in_line: at.i - at.line_start,
        }
    }

    /// The serializer's refusal of a value it could not convert, where the reader stood.
    fn convert_at(&self, at: Mark, type_name: &str) -> StjReadError {
        self.fail_at(
            at,
            format!("The JSON value could not be converted to {type_name}."),
        )
    }

    fn skip_ws(&mut self) {
        while let Some(&b) = self.s.get(self.i) {
            match b {
                b' ' | b'\t' | b'\r' => self.i += 1,
                b'\n' => {
                    self.i += 1;
                    self.line += 1;
                    self.line_start = self.i;
                }
                _ => break,
            }
        }
    }

    fn document(&mut self, shape: Option<&Shape>) -> Result<(), StjReadError> {
        self.skip_ws();
        if self.i >= self.s.len() {
            return Err(self.fail(
                "The input does not contain any JSON tokens. Expected the input to start with a valid JSON token, while isReadFinalBlock is true.",
            ));
        }
        match shape {
            None => self.value(0)?,
            Some(shape) => self.root(shape)?,
        }
        self.skip_ws();
        if let Some(&b) = self.s.get(self.i) {
            return Err(self.fail(format!(
                "'{}' is invalid after a single JSON value. Expected end of data.",
                printable(b)
            )));
        }
        Ok(())
    }

    /// The body itself: the record's object, or `null`. Anything else is read as one token
    /// and refused for the record's type.
    fn root(&mut self, shape: &Shape) -> Result<(), StjReadError> {
        match self.s[self.i] {
            b'{' => self.record(shape, 1),
            b'n' => self.value(0),
            b'[' => {
                self.i += 1;
                Err(self.convert_at(self.mark(), shape.name))
            }
            _ => {
                self.value(0)?;
                Err(self.convert_at(self.mark(), shape.name))
            }
        }
    }

    /// A member's value, read into `kind`. `owner` is the record whose constructor took it: a
    /// scalar the converter refuses names that record, as the serializer's message did for a
    /// constructor parameter; elsewhere (a class's setter, a list's element) it names the type.
    fn member(&mut self, kind: &Kind, owner: Option<&Shape>, depth: usize) -> Result<(), StjReadError> {
        let scalar_name = || {
            owner
                .filter(|shape| shape.by_constructor)
                .map_or_else(|| kind.clr_name(), |shape| shape.name.to_string())
        };
        let first = self.s[self.i];
        match kind {
            Kind::Bool | Kind::Int32 | Kind::Int64 | Kind::OptionalInt32 | Kind::Text => {
                if matches!(first, b'{' | b'[') {
                    // A value converter's input is read ahead whole, then the converter refuses
                    // the container's first token.
                    self.i += 1;
                    let after_open = self.mark();
                    self.i -= 1;
                    self.value(depth)?;
                    return Err(self.convert_at(after_open, &scalar_name()));
                }
                let start = self.i;
                self.value(depth)?;
                let token = &self.s[start..self.i];
                let fits = match (kind, first) {
                    (_, b'n') => matches!(kind, Kind::Text | Kind::OptionalInt32),
                    (Kind::Bool, _) => matches!(first, b't' | b'f'),
                    (Kind::Text, _) => first == b'"',
                    (_, b'"') => {
                        let text: String = serde_json::from_slice(token).unwrap_or_default();
                        integer_fits(kind, &text)
                    }
                    (_, b'-' | b'0'..=b'9') => integer_fits(kind, &String::from_utf8_lossy(token)),
                    _ => false,
                };
                if fits {
                    Ok(())
                } else if first == b'n' {
                    // A null for a value type: the serializer names the type itself.
                    Err(self.convert_at(self.mark(), &kind.clr_name()))
                } else {
                    Err(self.convert_at(self.mark(), &scalar_name()))
                }
            }
            Kind::TextList | Kind::RecordList(_) => match first {
                b'n' => self.value(depth),
                b'[' => {
                    let element = match kind {
                        Kind::RecordList(shape) => ElementKind::Record(shape),
                        _ => ElementKind::Text,
                    };
                    self.list(element, depth + 1)
                }
                _ => self.refuse_container(&kind.clr_name(), depth),
            },
            Kind::TextMap => match first {
                b'n' => self.value(depth),
                b'{' => self.map(depth + 1),
                _ => self.refuse_container(&kind.clr_name(), depth),
            },
            Kind::Record(shape) => match first {
                b'n' => self.value(depth),
                b'{' => self.record(shape, depth + 1),
                _ => self.refuse_container(shape.name, depth),
            },
        }
    }

    /// A list, dictionary or record converter given the wrong token: it refuses it as soon as it
    /// is read (an opening bracket at once, a scalar once read).
    fn refuse_container(&mut self, type_name: &str, depth: usize) -> Result<(), StjReadError> {
        if matches!(self.s[self.i], b'{' | b'[') {
            self.i += 1;
        } else {
            self.value(depth)?;
        }
        Err(self.convert_at(self.mark(), type_name))
    }

    /// After a member's or element's comma: a value must follow.
    fn after_comma(&mut self, close: u8) -> Result<(), StjReadError> {
        self.i += 1;
        if self.i >= self.s.len() {
            // The reader steps back onto the comma to say so.
            let mut at = self.mark();
            at.i -= 1;
            return Err(self.fail_at(at, NOTHING_AFTER_COMMA));
        }
        self.skip_ws();
        match self.s.get(self.i) {
            None => Err(self.fail(NOTHING_AFTER_COMMA)),
            Some(&b) if b == close => Err(self.fail(if close == b'}' {
                "The JSON object contains a trailing comma at the end which is not supported in this mode. Change the reader options."
            } else {
                "The JSON array contains a trailing comma at the end which is not supported in this mode. Change the reader options."
            })),
            Some(_) => Ok(()),
        }
    }

    /// After a member or element: a comma, the closing bracket or the end.
    /// Ok(true) when the container closed.
    fn after_value(&mut self, close: u8) -> Result<bool, StjReadError> {
        self.skip_ws();
        match self.s.get(self.i) {
            Some(b',') => {
                self.after_comma(close)?;
                Ok(false)
            }
            Some(&b) if b == close => {
                self.i += 1;
                Ok(true)
            }
            None => Err(self.fail(ZERO_DEPTH_AT_END)),
            Some(&b) => Err(self.fail(format!(
                "'{}' is invalid after a value. Expected either ',', '}}', or ']'.",
                printable(b)
            ))),
        }
    }

    /// A property name and its colon: the name as written.
    fn property_name(&mut self) -> Result<String, StjReadError> {
        match self.s.get(self.i) {
            None => return Err(self.fail(ZERO_DEPTH_AT_END)),
            Some(b'"') => {}
            Some(&b) => {
                return Err(self.fail(format!(
                    "'{}' is an invalid start of a property name. Expected a '\"'.",
                    printable(b)
                )));
            }
        }
        let start = self.i;
        self.string()?;
        let name: String = serde_json::from_slice(&self.s[start..self.i]).unwrap_or_default();
        self.skip_ws();
        match self.s.get(self.i) {
            Some(b':') => self.i += 1,
            None => return Err(self.fail("Expected a value, but instead reached end of data.")),
            Some(&b) => {
                return Err(self.fail(format!(
                    "'{}' is invalid after a property name. Expected a ':'.",
                    printable(b)
                )));
            }
        }
        self.skip_ws();
        Ok(name)
    }

    /// An object read into a record: each member's value as its kind, others skipped.
    fn record(&mut self, shape: &Shape, depth: usize) -> Result<(), StjReadError> {
        self.check_depth(depth)?;
        self.i += 1;
        self.skip_ws();
        if self.s.get(self.i) == Some(&b'}') {
            self.i += 1;
            return Ok(());
        }
        loop {
            let name = self.property_name()?;
            let member = shape.members.iter().find(|m| m.name.eq_ignore_ascii_case(&name));
            self.path.push(Step::Name(name));
            if self.i >= self.s.len() {
                return Err(self.fail(ZERO_DEPTH_AT_END));
            }
            match member {
                Some(member) => self.member(&member.kind, Some(shape), depth)?,
                None => self.value(depth)?,
            }
            self.path.pop();
            if self.after_value(b'}')? {
                return Ok(());
            }
        }
    }

    /// A `Dictionary<string, string>`: each key on the path while its value is read.
    fn map(&mut self, depth: usize) -> Result<(), StjReadError> {
        self.check_depth(depth)?;
        self.i += 1;
        self.skip_ws();
        if self.s.get(self.i) == Some(&b'}') {
            self.i += 1;
            return Ok(());
        }
        loop {
            let name = self.property_name()?;
            self.path.push(Step::Name(name));
            if self.i >= self.s.len() {
                return Err(self.fail(ZERO_DEPTH_AT_END));
            }
            self.member(&Kind::Text, None, depth)?;
            self.path.pop();
            if self.after_value(b'}')? {
                return Ok(());
            }
        }
    }

    /// A list: the path names the element being read (the count read so far).
    fn list(&mut self, element: ElementKind, depth: usize) -> Result<(), StjReadError> {
        self.check_depth(depth)?;
        self.i += 1;
        self.path.push(Step::Index(0));
        self.skip_ws();
        if self.s.get(self.i) == Some(&b']') {
            self.i += 1;
            self.path.pop();
            return Ok(());
        }
        let mut count = 0;
        loop {
            if self.i >= self.s.len() {
                return Err(self.fail(ZERO_DEPTH_AT_END));
            }
            match element {
                ElementKind::Text => self.member(&Kind::Text, None, depth)?,
                ElementKind::Record(shape) => match self.s[self.i] {
                    b'{' => self.record(shape, depth + 1)?,
                    b'n' => self.value(depth)?,
                    _ => self.refuse_container(shape.name, depth)?,
                },
            }
            count += 1;
            if let Some(last) = self.path.last_mut() {
                *last = Step::Index(count);
            }
            if self.after_value(b']')? {
                self.path.pop();
                return Ok(());
            }
        }
    }

    /// Any value, read for its syntax only (the serializer skipping a member it does not have).
    fn value(&mut self, depth: usize) -> Result<(), StjReadError> {
        let Some(&b) = self.s.get(self.i) else {
            return Err(self.fail(ZERO_DEPTH_AT_END));
        };
        match b {
            b'{' => self.object(depth + 1),
            b'[' => self.array(depth + 1),
            b'"' => self.string(),
            b'-' | b'0'..=b'9' => self.number(),
            b't' => self.literal("true"),
            b'f' => self.literal("false"),
            b'n' => self.literal("null"),
            _ => Err(self.fail(format!("'{}' is an invalid start of a value.", printable(b)))),
        }
    }

    fn check_depth(&self, depth: usize) -> Result<(), StjReadError> {
        if depth > MAX_DEPTH {
            return Err(self.fail(format!(
                "The maximum configured depth of {MAX_DEPTH} has been exceeded. Cannot read next JSON object."
            )));
        }
        Ok(())
    }

    fn object(&mut self, depth: usize) -> Result<(), StjReadError> {
        self.check_depth(depth)?;
        self.i += 1;
        self.skip_ws();
        if self.s.get(self.i) == Some(&b'}') {
            self.i += 1;
            return Ok(());
        }
        loop {
            self.property_name()?;
            if self.i >= self.s.len() {
                return Err(self.fail(ZERO_DEPTH_AT_END));
            }
            self.value(depth)?;
            if self.after_value(b'}')? {
                return Ok(());
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<(), StjReadError> {
        self.check_depth(depth)?;
        self.i += 1;
        self.skip_ws();
        if self.s.get(self.i) == Some(&b']') {
            self.i += 1;
            return Ok(());
        }
        loop {
            if self.i >= self.s.len() {
                return Err(self.fail(ZERO_DEPTH_AT_END));
            }
            self.value(depth)?;
            if self.after_value(b']')? {
                return Ok(());
            }
        }
    }

    fn string(&mut self) -> Result<(), StjReadError> {
        self.i += 1;
        loop {
            let Some(&b) = self.s.get(self.i) else {
                return Err(self.fail("Expected end of string, but instead reached end of data."));
            };
            match b {
                b'"' => {
                    self.i += 1;
                    return Ok(());
                }
                b'\\' => {
                    self.i += 1;
                    match self.s.get(self.i) {
                        Some(b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't') => self.i += 1,
                        Some(b'u') => {
                            self.i += 1;
                            for _ in 0..4 {
                                match self.s.get(self.i) {
                                    Some(h) if h.is_ascii_hexdigit() => self.i += 1,
                                    Some(&h) => {
                                        return Err(self.fail(format!(
                                            "'{}' is not a hex digit following '\\u' within a JSON string. The string should be correctly escaped.",
                                            printable(h)
                                        )));
                                    }
                                    None => {
                                        return Err(self.fail(
                                            "Expected end of string, but instead reached end of data.",
                                        ));
                                    }
                                }
                            }
                        }
                        Some(&e) => {
                            return Err(self.fail(format!(
                                "'{}' is an invalid escapable character within a JSON string. The string should be correctly escaped.",
                                printable(e)
                            )));
                        }
                        None => {
                            return Err(self.fail("Expected end of string, but instead reached end of data."));
                        }
                    }
                }
                0x00..=0x1f => {
                    return Err(self.fail(format!(
                        "'{}' is invalid within a JSON string. The string should be correctly escaped.",
                        printable(b)
                    )));
                }
                _ => self.i += 1,
            }
        }
    }

    fn digits(&mut self) -> usize {
        let start = self.i;
        while self.s.get(self.i).is_some_and(u8::is_ascii_digit) {
            self.i += 1;
        }
        self.i - start
    }

    fn number(&mut self) -> Result<(), StjReadError> {
        if self.s[self.i] == b'-' {
            self.i += 1;
            match self.s.get(self.i) {
                Some(b) if b.is_ascii_digit() => {}
                Some(&b) => {
                    return Err(self.fail(format!(
                        "'{}' is invalid within a number, immediately after a sign character ('+' or '-'). Expected a digit ('0'-'9').",
                        printable(b)
                    )));
                }
                None => return Err(self.fail("Expected a digit ('0'-'9'), but instead reached end of data.")),
            }
        }
        if self.s[self.i] == b'0' {
            self.i += 1;
            if let Some(&b) = self.s.get(self.i)
                && b.is_ascii_digit()
            {
                return Err(self.fail(format!("Invalid leading zero before '{}'.", printable(b))));
            }
        } else {
            self.digits();
        }
        if self.s.get(self.i) == Some(&b'.') {
            self.i += 1;
            match self.s.get(self.i) {
                Some(b) if b.is_ascii_digit() => {
                    self.digits();
                }
                Some(&b) => {
                    return Err(self.fail(format!(
                        "'{}' is invalid within a number, immediately after a decimal point ('.'). Expected a digit ('0'-'9').",
                        printable(b)
                    )));
                }
                None => return Err(self.fail("Expected a digit ('0'-'9'), but instead reached end of data.")),
            }
        }
        if matches!(self.s.get(self.i), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.s.get(self.i), Some(b'+' | b'-')) {
                self.i += 1;
            }
            match self.s.get(self.i) {
                Some(b) if b.is_ascii_digit() => {
                    self.digits();
                }
                Some(&b) => {
                    return Err(self.fail(format!(
                        "'{}' is invalid within a number, immediately after a sign character ('+' or '-'). Expected a digit ('0'-'9').",
                        printable(b)
                    )));
                }
                None => return Err(self.fail("Expected a digit ('0'-'9'), but instead reached end of data.")),
            }
        }
        match self.s.get(self.i) {
            None | Some(b' ' | b'\t' | b'\r' | b'\n' | b',' | b'}' | b']') => Ok(()),
            Some(&b) => Err(self.fail(format!(
                "'{}' is an invalid end of a number. Expected a delimiter.",
                printable(b)
            ))),
        }
    }

    fn literal(&mut self, word: &str) -> Result<(), StjReadError> {
        let bytes = word.as_bytes();
        let available = &self.s[self.i..];
        if available.starts_with(bytes) {
            self.i += bytes.len();
            return Ok(());
        }
        // The reader names what it found, up to the literal's length.
        let found: String = available
            .iter()
            .take(bytes.len())
            .map(|b| printable(*b))
            .collect();
        Err(self.fail(format!(
            "'{found}' is an invalid JSON literal. Expected the literal '{word}'."
        )))
    }
}

#[derive(Clone, Copy)]
enum ElementKind<'a> {
    Text,
    Record(&'a Shape),
}

/// Whether an integer member takes this text: digits with an optional sign and nothing else,
/// within the type's range.
fn integer_fits(kind: &Kind, text: &str) -> bool {
    match kind {
        Kind::Int64 => text.parse::<i64>().is_ok(),
        _ => text.parse::<i32>().is_ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reader_messages_name_the_first_offending_byte() {
        let cases = [
            (
                "{not json",
                "'n' is an invalid start of a property name. Expected a '\"'. LineNumber: 0 | BytePositionInLine: 1.",
            ),
            (
                "[1,]",
                "The JSON array contains a trailing comma at the end which is not supported in this mode. Change the reader options. LineNumber: 0 | BytePositionInLine: 3.",
            ),
            (
                "{\"a\":1}x",
                "'x' is invalid after a single JSON value. Expected end of data. LineNumber: 0 | BytePositionInLine: 7.",
            ),
            (
                "{\n  \"a\": x}",
                "'x' is an invalid start of a value. LineNumber: 1 | BytePositionInLine: 7.",
            ),
            (
                "{\"a\":1",
                "Expected depth to be zero at the end of the JSON payload. There is an open JSON object or array that should be closed. LineNumber: 0 | BytePositionInLine: 6.",
            ),
            // The end of data after a member's colon is the open object, as after a value.
            (
                "{\"scope\": ",
                "Expected depth to be zero at the end of the JSON payload. There is an open JSON object or array that should be closed. LineNumber: 0 | BytePositionInLine: 10.",
            ),
            // Before the colon, the reader still wants the member's value.
            (
                "{\"scope\"",
                "Expected a value, but instead reached end of data. LineNumber: 0 | BytePositionInLine: 8.",
            ),
            // After a comma it wants another member or element; with nothing after the comma it
            // steps back onto it.
            (
                "{\"a\": 1,",
                "Expected start of a property name or value, but instead reached end of data. LineNumber: 0 | BytePositionInLine: 7.",
            ),
            (
                "[1, ",
                "Expected start of a property name or value, but instead reached end of data. LineNumber: 0 | BytePositionInLine: 4.",
            ),
        ];
        for (input, expected) in cases {
            let error = stj_parse_error(input).unwrap_or_else(|| panic!("{input} is refused"));
            assert_eq!(
                format!("{} {}", error.message, error.position()),
                expected,
                "{input}"
            );
            assert_eq!(error.path, "$", "{input}");
        }
        for valid in [
            "{}",
            "[]",
            "null",
            " {\"a\": [1, -2.5e3, true, false, null, \"\\u00e9\"]} ",
        ] {
            assert_eq!(stj_parse_error(valid), None, "{valid}");
        }
    }

    const STEP: &Shape = &Shape {
        name: "Octo.Controllers.LibraryHealthController+StepRequest",
        by_constructor: true,
        members: &[
            Member {
                name: "action",
                kind: Kind::Text,
            },
            Member {
                name: "with",
                kind: Kind::TextMap,
            },
        ],
    };
    const RECORD: &Shape = &Shape {
        name: "Octo.Controllers.Test+Record",
        by_constructor: true,
        members: &[
            Member {
                name: "scope",
                kind: Kind::Text,
            },
            Member {
                name: "on",
                kind: Kind::Bool,
            },
            Member {
                name: "count",
                kind: Kind::Int32,
            },
            Member {
                name: "keys",
                kind: Kind::TextList,
            },
            Member {
                name: "steps",
                kind: Kind::RecordList(STEP),
            },
        ],
    };
    const CLASS: &Shape = &Shape {
        name: "Octo.Controllers.Test+Class",
        by_constructor: false,
        members: &[
            Member {
                name: "user",
                kind: Kind::Text,
            },
            Member {
                name: "size",
                kind: Kind::Int64,
            },
            Member {
                name: "rate",
                kind: Kind::OptionalInt32,
            },
        ],
    };

    fn refusal(shape: &Shape, body: &str) -> Option<(String, String)> {
        read_error(body, shape).map(|e| (e.path.clone(), format!("{} {}", e.message, e.position())))
    }

    fn converted(type_name: &str, line: usize, byte: usize) -> String {
        format!(
            "The JSON value could not be converted to {type_name}. LineNumber: {line} | BytePositionInLine: {byte}."
        )
    }

    #[test]
    fn a_broken_body_names_the_member_the_serializer_had_reached() {
        let depth = ZERO_DEPTH_AT_END;
        for (body, path, message) in [
            // The reference recording (17-covers-explicit/covers-start-bad-json).
            ("{\"scope\": ", "$.scope", format!("{depth} LineNumber: 0 | BytePositionInLine: 10.")),
            // A finished member is off the path again.
            ("{\"scope\": \"a\" x", "$", "'x' is invalid after a value. Expected either ',', '}', or ']'. LineNumber: 0 | BytePositionInLine: 14.".to_string()),
            // A member it does not have is still named, as written.
            ("{\"Other\": [1, ", "$.Other", "Expected start of a property name or value, but instead reached end of data. LineNumber: 0 | BytePositionInLine: 14.".to_string()),
            // A list names the element under way.
            ("{\"keys\": [\"a\", \"b", "$.keys[1]", "Expected end of string, but instead reached end of data. LineNumber: 0 | BytePositionInLine: 17.".to_string()),
            ("{\"keys\": [", "$.keys[0]", format!("{depth} LineNumber: 0 | BytePositionInLine: 10.")),
            // Records inside lists, and a dictionary's key.
            ("{\"steps\": [{}, {\"with\": {\"a b\": ", "$.steps[1].with['a b']", format!("{depth} LineNumber: 0 | BytePositionInLine: 32.")),
            // The property name itself is not finished: the path is the object's.
            ("{\"sco", "$", "Expected end of string, but instead reached end of data. LineNumber: 0 | BytePositionInLine: 5.".to_string()),
        ] {
            assert_eq!(refusal(RECORD, body), Some((path.to_string(), message)), "{body}");
        }
    }

    #[test]
    fn a_member_of_the_wrong_type_is_refused_where_the_reader_stood() {
        let record = RECORD.name;
        for (body, path, message) in [
            // The reference answer to {"on": "yes"} for an OnRequest (12-imports/fetch-bad-bool).
            ("{\"on\": \"yes\"}", "$.on", converted(record, 0, 12)),
            // Named as written, after a member it skipped.
            (
                "{\"x\":[1,{\"on\":2}],\"On\":12}",
                "$.On",
                converted(record, 0, 25),
            ),
            // A null for a value type names the type itself.
            ("{\n  \"on\": null\n}", "$.on", converted("System.Boolean", 1, 12)),
            // A container is read whole first, then refused at its first token.
            ("{\"on\": {\"a\": 1}}", "$.on", converted(record, 0, 8)),
            ("{\"scope\": 5}", "$.scope", converted(record, 0, 11)),
            ("{\"count\": \"12x\"}", "$.count", converted(record, 0, 15)),
            ("{\"count\": 1.5}", "$.count", converted(record, 0, 13)),
            // Lists refuse the wrong token for their own type, elements for theirs.
            (
                "{\"keys\": \"a\"}",
                "$.keys",
                converted("System.Collections.Generic.List`1[System.String]", 0, 12),
            ),
            (
                "{\"keys\": {\"a\": 1}}",
                "$.keys",
                converted("System.Collections.Generic.List`1[System.String]", 0, 10),
            ),
            (
                "{\"keys\": [\"a\", 2]}",
                "$.keys[1]",
                converted("System.String", 0, 16),
            ),
            (
                "{\"steps\": [5]}",
                "$.steps[0]",
                converted("Octo.Controllers.LibraryHealthController+StepRequest", 0, 12),
            ),
            (
                "{\"steps\": [{\"action\": true}]}",
                "$.steps[0].action",
                converted("Octo.Controllers.LibraryHealthController+StepRequest", 0, 26),
            ),
            (
                "{\"steps\": [{\"with\": {\"a\": 1}}]}",
                "$.steps[0].with.a",
                converted("System.String", 0, 27),
            ),
            // The body itself must be the record's object.
            ("[1]", "$", converted(record, 0, 1)),
            ("\"text\"", "$", converted(record, 0, 6)),
            // The first problem in reading order wins.
            ("{\"on\": \"yes\", x", "$.on", converted(record, 0, 12)),
        ] {
            assert_eq!(refusal(RECORD, body), Some((path.to_string(), message)), "{body}");
        }
        // A class's setter names the member's type.
        assert_eq!(
            refusal(CLASS, "{\"user\": 5}"),
            Some(("$.user".to_string(), converted("System.String", 0, 10)))
        );
        assert_eq!(
            refusal(CLASS, "{\"size\": \"big\"}"),
            Some(("$.size".to_string(), converted("System.Int64", 0, 14)))
        );
        assert_eq!(
            refusal(CLASS, "{\"rate\": true}"),
            Some((
                "$.rate".to_string(),
                converted("System.Nullable`1[System.Int32]", 0, 13)
            ))
        );
        for fine in [
            "null",
            "{}",
            "{\"on\": true, \"count\": \"12\", \"keys\": null, \"scope\": null, \"other\": {\"x\": [1]}}",
            "{\"KEYS\": [\"a\", null], \"steps\": [null, {\"with\": {\"a\": null}}], \"Count\": -3}",
        ] {
            assert_eq!(refusal(RECORD, fine), None, "{fine}");
        }
        assert_eq!(refusal(CLASS, "{\"size\": \"12\", \"rate\": null}"), None);
    }
}
