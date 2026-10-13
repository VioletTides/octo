//! The one parameter dictionary every Subsonic action works from, plus the request collections
//! it was built from, which the relay needs again to send repeated keys as the client sent them.
//!
//! The parser reads a [`RequestParts`] (raw query string, content type, content length and the
//! body bytes), so it stays free of any HTTP stack. The rules:
//!
//! - **Query**: split on `&` only (never `;`), empty segments skipped, a segment without `=` is a
//!   key with an empty value, `=v` is the empty key. `+` is a space, then `%XX` is decoded as
//!   UTF-8; an escape that is not valid UTF-8 (or not an escape at all) is left as written
//!   (`%FF`, `%ZZ`, a lone `%`). Keys are matched **ignoring case**, keep the spelling they were
//!   first sent with, and collect every value (`u=a&U=b` is one key `u` holding `a` and `b`).
//! - **The dictionary** is ordinal (`F` is not `f`), keeps insertion order, and a repeated key
//!   becomes its values joined with commas, leaving out the empty ones (`id=&id=B` reads `B`).
//! - **Body**, read only when `Content-Length > 0` or a `Content-Type` is present:
//!   - a form (`application/x-www-form-urlencoded` or `multipart/form-data`, the media type
//!     matched exactly, ignoring case): every text field, file parts left out. When the
//!     form reader fails before reading anything (a multipart type with no usable boundary), the
//!     body is parsed as a query string instead; when it fails part way, nothing comes of it.
//!   - otherwise a content type that contains `application/json` (ordinal, case-sensitive): a
//!     top-level object, each value as text (strings unquoted, numbers,
//!     objects and arrays as their raw text, `True`/`False`, null as empty). Anything that is
//!     not one object with at most 64 levels of nesting is ignored entirely.
//!
//!   Body values override query values; an overridden key keeps its first place.

use indexmap::IndexMap;
use octo_core::common::dotnet;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};

/// The parameter dictionary: keys compared ordinally, insertion order kept, and an overwrite
/// keeps the key where it first appeared.
pub type Parameters = IndexMap<String, String>;

/// The parts of an HTTP request the parser reads.
#[derive(Debug, Clone, Copy, Default)]
pub struct RequestParts<'a> {
    /// The raw query string, without its leading `?` (as `Uri::query` gives it).
    pub query: Option<&'a str>,
    pub content_type: Option<&'a str>,
    pub content_length: Option<u64>,
    pub body: &'a [u8],
}

/// A request's values by name, from its query or its form: keys matched ignoring case,
/// spelled as first sent, in the order first sent, each with every value it was given.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StringValuesCollection {
    entries: Vec<(String, Vec<String>)>,
}

impl StringValuesCollection {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds one value under `key`, joining a key already present in any casing.
    pub fn append(&mut self, key: impl Into<String>, value: impl Into<String>) {
        let key = key.into();
        let value = value.into();
        match self
            .entries
            .iter_mut()
            .find(|(k, _)| dotnet::eq_ignore_case(k, &key))
        {
            Some((_, values)) => values.push(value),
            None => self.entries.push((key, vec![value])),
        }
    }

    /// Every value sent under `key`, matched ignoring case.
    pub fn get(&self, key: &str) -> Option<&[String]> {
        self.entries
            .iter()
            .find(|(k, _)| dotnet::eq_ignore_case(k, key))
            .map(|(_, v)| v.as_slice())
    }

    /// The keys as first spelled, with their values, in the order first sent.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &[String])> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v.as_slice()))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// The values joined with commas, empty ones left out.
pub fn join_values(values: &[String]) -> String {
    let mut out = String::new();
    for value in values.iter().filter(|v| !v.is_empty()) {
        if !out.is_empty() {
            out.push(',');
        }
        out.push_str(value);
    }
    out
}

/// The query string by the rules in the module docs. The text is the query without its `?`;
/// a second `?` is part of the first key.
pub fn parse_query(raw: &str) -> StringValuesCollection {
    let mut collection = StringValuesCollection::new();
    for segment in raw.split('&').filter(|s| !s.is_empty()) {
        let (name, value) = segment.split_once('=').unwrap_or((segment, ""));
        collection.append(decode_component(name), decode_component(value));
    }
    collection
}

/// The same parse, after dropping one leading `?`. The form reader's fallback parses a body this
/// way.
pub fn parse_query_helpers(text: &str) -> StringValuesCollection {
    parse_query(text.strip_prefix('?').unwrap_or(text))
}

/// `+` as a space, then [`unescape_data_string`].
fn decode_component(text: &str) -> String {
    unescape_data_string(&text.replace('+', " "))
}

/// Percent-decoding: each run of `%XX` escapes is decoded as UTF-8, and any escape that does not
/// form valid UTF-8 is left as written. A `%` not followed by two hex digits is left alone too.
pub fn unescape_data_string(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < bytes.len() {
        if !is_escape(bytes, i) {
            // Copy the plain run up to the next escape whole, which keeps multi-byte
            // characters intact.
            let start = i;
            i += 1;
            while i < bytes.len() && !is_escape(bytes, i) {
                i += 1;
            }
            out.push_str(&text[start..i]);
            continue;
        }
        // A run of escapes, decoded together so a multi-byte character spelled as several
        // escapes comes out as that character.
        let start = i;
        let mut decoded = Vec::new();
        while is_escape(bytes, i) {
            decoded.push(hex_value(bytes[i + 1]) * 16 + hex_value(bytes[i + 2]));
            i += 3;
        }
        let mut offset = 0;
        for chunk in decoded.utf8_chunks() {
            out.push_str(chunk.valid());
            offset += chunk.valid().len();
            for _ in chunk.invalid() {
                // The escape exactly as it was written, `%ff` stays lower case.
                let at = start + offset * 3;
                out.push_str(&text[at..at + 3]);
                offset += 1;
            }
        }
    }
    out
}

fn is_escape(bytes: &[u8], i: usize) -> bool {
    i + 2 < bytes.len()
        && bytes[i] == b'%'
        && bytes[i + 1].is_ascii_hexdigit()
        && bytes[i + 2].is_ascii_hexdigit()
}

fn hex_value(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        _ => b - b'A' + 10,
    }
}

/// The characters [`escape_data_string`] leaves alone: the RFC 3986 unreserved set.
const DATA_STRING: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

/// Percent-encoding: everything but `A-Z a-z 0-9 - _ . ~` as upper-case `%XX` of its
/// UTF-8 bytes.
pub fn escape_data_string(text: &str) -> String {
    utf8_percent_encode(text, DATA_STRING).to_string()
}

/// Whether the media type, parameters aside, is exactly a form type, ignoring case.
pub fn has_form_content_type(content_type: Option<&str>) -> bool {
    let Some(content_type) = content_type else {
        return false;
    };
    let media_type = content_type.split(';').next().unwrap_or("").trim();
    media_type.eq_ignore_ascii_case("application/x-www-form-urlencoded")
        || media_type.eq_ignore_ascii_case("multipart/form-data")
}

/// How [`read_form`] failed, which decides what its callers' fallback could still read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FormError {
    /// It failed before reading the body (a multipart type with no usable boundary), so the
    /// whole body was still there for the fallback.
    #[error("{0}")]
    BeforeReading(String),
    /// It failed part way through, after consuming the body.
    #[error("{0}")]
    WhileReading(String),
}

/// The most values one form may hold.
const VALUE_COUNT_LIMIT: usize = 1024;
/// The longest form key, in bytes.
const KEY_LENGTH_LIMIT: usize = 2048;
/// The longest form value, in bytes.
const VALUE_LENGTH_LIMIT: usize = 4 * 1024 * 1024;
/// The longest multipart boundary.
const BOUNDARY_LENGTH_LIMIT: usize = 70;

/// The form of a request whose content type is a form type: the text fields (file parts are not
/// form values), keys matched ignoring case. The body is read as UTF-8.
pub fn read_form(content_type: &str, body: &[u8]) -> Result<StringValuesCollection, FormError> {
    let media_type = content_type.split(';').next().unwrap_or("").trim();
    if media_type.eq_ignore_ascii_case("multipart/form-data") {
        read_multipart(content_type, body)
    } else {
        read_url_encoded(body)
    }
}

fn read_url_encoded(body: &[u8]) -> Result<StringValuesCollection, FormError> {
    let text = String::from_utf8_lossy(body);
    let mut collection = StringValuesCollection::new();
    let mut count = 0;
    for segment in text.split('&').filter(|s| !s.is_empty()) {
        let (name, value) = segment.split_once('=').unwrap_or((segment, ""));
        count += 1;
        if count > VALUE_COUNT_LIMIT {
            return Err(FormError::WhileReading(format!(
                "Form value count limit {VALUE_COUNT_LIMIT} exceeded."
            )));
        }
        if name.len() > KEY_LENGTH_LIMIT {
            return Err(FormError::WhileReading(format!(
                "Form key length limit {KEY_LENGTH_LIMIT} exceeded."
            )));
        }
        if value.len() > VALUE_LENGTH_LIMIT {
            return Err(FormError::WhileReading(format!(
                "Form value length limit {VALUE_LENGTH_LIMIT} exceeded."
            )));
        }
        collection.append(decode_component(name), decode_component(value));
    }
    Ok(collection)
}

/// One parameter of a header value (`boundary=...`, `name="..."`), quotes removed, the name
/// matched ignoring case.
fn header_parameter(value: &str, name: &str) -> Option<String> {
    value.split(';').skip(1).find_map(|part| {
        let (key, val) = part.split_once('=')?;
        if !key.trim().eq_ignore_ascii_case(name) {
            return None;
        }
        let val = val.trim();
        let unquoted = if val.len() >= 2 && val.starts_with('"') && val.ends_with('"') {
            &val[1..val.len() - 1]
        } else {
            val
        };
        Some(unquoted.to_string())
    })
}

fn read_multipart(content_type: &str, body: &[u8]) -> Result<StringValuesCollection, FormError> {
    let boundary = match header_parameter(content_type, "boundary") {
        Some(b) if !b.trim().is_empty() => b,
        _ => return Err(FormError::BeforeReading("Missing content-type boundary.".into())),
    };
    if boundary.len() > BOUNDARY_LENGTH_LIMIT {
        return Err(FormError::BeforeReading(format!(
            "Multipart boundary length limit {BOUNDARY_LENGTH_LIMIT} exceeded."
        )));
    }
    let unexpected_end = || {
        FormError::WhileReading(
            "Unexpected end of Stream, the content may have already been read by another component. ".into(),
        )
    };
    let delimiter = format!("--{boundary}").into_bytes();
    let mut collection = StringValuesCollection::new();
    let mut count = 0;

    // The preamble runs up to the first delimiter.
    let mut at = find(body, &delimiter, 0).ok_or_else(unexpected_end)? + delimiter.len();
    loop {
        // The rest of the delimiter line: "--" closes the body, otherwise a line break.
        if body[at..].starts_with(b"--") {
            return Ok(collection);
        }
        let line_end = find(body, b"\r\n", at).ok_or_else(unexpected_end)?;
        let headers_start = line_end + 2;
        // A part with no headers starts with the blank line straight away.
        let headers_end = if body[headers_start..].starts_with(b"\r\n") {
            (headers_start, headers_start + 2)
        } else {
            find(body, b"\r\n\r\n", headers_start)
                .map(|i| (i, i + 4))
                .ok_or_else(unexpected_end)?
        };
        let headers = String::from_utf8_lossy(&body[headers_start..headers_end.0]);
        let content_start = headers_end.1;
        let mut close = b"\r\n".to_vec();
        close.extend_from_slice(&delimiter);
        let content_end = find(body, &close, content_start).ok_or_else(unexpected_end)?;
        at = content_end + close.len();

        let disposition = headers.split("\r\n").find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("Content-Disposition")
                .then(|| value.trim().to_string())
        });
        let Some(disposition) = disposition else {
            continue;
        };
        let kind = disposition.split(';').next().unwrap_or("").trim();
        if !kind.eq_ignore_ascii_case("form-data") {
            continue;
        }
        let is_file = ["filename", "filename*"]
            .iter()
            .any(|p| header_parameter(&disposition, p).is_some_and(|v| !v.is_empty()));
        if is_file {
            continue;
        }
        let name = header_parameter(&disposition, "name").unwrap_or_default();
        count += 1;
        if count > VALUE_COUNT_LIMIT {
            return Err(FormError::WhileReading(format!(
                "Form value count limit {VALUE_COUNT_LIMIT} exceeded."
            )));
        }
        let value = String::from_utf8_lossy(&body[content_start..content_end]).into_owned();
        collection.append(name, value);
    }
}

fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from > haystack.len() || needle.is_empty() {
        return None;
    }
    haystack[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|i| i + from)
}

/// A form body's parameters: [`read_form`], or when that fails, what was left of the body
/// parsed as a query string.
fn form_parameters(content_type: &str, body: &[u8]) -> StringValuesCollection {
    match read_form(content_type, body) {
        Ok(form) => form,
        Err(FormError::BeforeReading(_)) => {
            let text = String::from_utf8_lossy(body);
            if text.is_empty() {
                StringValuesCollection::new()
            } else {
                parse_query_helpers(&text)
            }
        }
        // The reader had consumed the body, so the fallback read nothing.
        Err(FormError::WhileReading(_)) => StringValuesCollection::new(),
    }
}

/// The deepest nesting a JSON body may have.
const MAX_JSON_DEPTH: usize = 64;

/// A JSON object body's members, each as its raw JSON text, read the way the parameters read
/// one: UTF-8 with replacement characters, a byte order mark dropped, at most 64 levels deep.
/// `None` when the body is not one such object.
pub fn json_object(body: &[u8]) -> Option<IndexMap<String, Box<serde_json::value::RawValue>>> {
    let text = String::from_utf8_lossy(body);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    if text.is_empty() {
        return None;
    }
    let members: IndexMap<String, Box<serde_json::value::RawValue>> = serde_json::from_str(text).ok()?;
    // The object itself is one level.
    if members.values().any(|raw| depth(raw.get()) + 1 > MAX_JSON_DEPTH) {
        return None;
    }
    Some(members)
}

/// A JSON body's parameters: a top-level object's members as text (see the module docs), or
/// `None` when the body is not one, which adds no parameters.
pub fn json_parameters(body: &[u8]) -> Option<Vec<(String, String)>> {
    // UTF-8 with replacement characters, a byte order mark dropped.
    let members = json_object(body)?;
    let mut out = Vec::with_capacity(members.len());
    for (key, raw) in members {
        let raw = raw.get();
        let value = match raw.as_bytes().first() {
            Some(b'"') => serde_json::from_str::<String>(raw).ok()?,
            Some(b't') => "True".to_string(),
            Some(b'f') => "False".to_string(),
            // A JSON null is an empty value.
            Some(b'n') => String::new(),
            // Numbers, objects and arrays: their raw text, whitespace and all.
            _ => raw.to_string(),
        };
        out.push((key, value));
    }
    Some(out)
}

/// The deepest nesting of objects and arrays in a JSON value.
fn depth(raw: &str) -> usize {
    let (mut current, mut deepest, mut in_string, mut escaped) = (0usize, 0usize, false, false);
    for b in raw.bytes() {
        if in_string {
            match b {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' | b'[' => {
                current += 1;
                deepest = deepest.max(current);
            }
            b'}' | b']' => current = current.saturating_sub(1),
            _ => {}
        }
    }
    deepest
}

/// Every parameter of a request: query values first, then the body's, which override them.
pub fn extract_all_parameters(request: &RequestParts<'_>) -> Parameters {
    let mut parameters = Parameters::new();
    for (key, values) in parse_query(request.query.unwrap_or("")).iter() {
        parameters.insert(key.to_string(), join_values(values));
    }

    if request.content_length.is_some_and(|l| l > 0) || request.content_type.is_some() {
        // Handle application/x-www-form-urlencoded (OpenSubsonic formPost extension)
        if has_form_content_type(request.content_type) {
            let content_type = request.content_type.unwrap_or("");
            for (key, values) in form_parameters(content_type, request.body).iter() {
                parameters.insert(key.to_string(), join_values(values));
            }
        }
        // Handle application/json
        else if request
            .content_type
            .is_some_and(|ct| ct.contains("application/json"))
            && let Some(members) = json_parameters(request.body)
        {
            for (key, value) in members {
                parameters.insert(key, value);
            }
        }
    }
    parameters
}

/// Every occurrence of a parameter, from the query and then the form, blank ones skipped. The
/// dictionary joins a repeated key into one comma-separated value for lookups; this keeps them
/// apart. The name is matched ignoring case, as the collections match it. A form that fails to
/// read adds nothing.
pub fn extract_parameter_values(request: &RequestParts<'_>, name: &str) -> Vec<String> {
    let mut values = Vec::new();
    let keep = |v: &&String| !dotnet::is_blank(v);
    if let Some(query) = parse_query(request.query.unwrap_or("")).get(name) {
        values.extend(query.iter().filter(keep).cloned());
    }
    if has_form_content_type(request.content_type)
        && let Ok(form) = read_form(request.content_type.unwrap_or(""), request.body)
        && let Some(body) = form.get(name)
    {
        values.extend(body.iter().filter(keep).cloned());
    }
    values
}

/// What a request's body came to, read strictly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BodyReading {
    /// Nothing to read: no body, or a body of a kind the parameters never read.
    Nothing,
    /// A url-encoded or multipart form's text fields.
    Form(StringValuesCollection),
    /// A JSON object's members, as [`json_parameters`] gives them.
    Json(Vec<(String, String)>),
    /// A form or JSON body that could not be read. [`extract_all_parameters`] would fall back
    /// to guessing at it, or leave it out.
    Unreadable,
}

/// A request read once, query and body: everything its parameters and its sign-in come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestReading {
    pub query: StringValuesCollection,
    pub body: BodyReading,
}

/// Reads a request's query and body once. A readable body comes to the same parameters as
/// [`extract_all_parameters`]; a form or JSON body that cannot be read is
/// [`BodyReading::Unreadable`] instead of a guess.
pub fn read_request(request: &RequestParts<'_>) -> RequestReading {
    let query = parse_query(request.query.unwrap_or(""));
    let considered = request.content_length.is_some_and(|l| l > 0) || request.content_type.is_some();
    let body = if !considered {
        BodyReading::Nothing
    } else if has_form_content_type(request.content_type) {
        match read_form(request.content_type.unwrap_or(""), request.body) {
            Ok(form) => BodyReading::Form(form),
            Err(_) if request.body.is_empty() => BodyReading::Nothing,
            Err(_) => BodyReading::Unreadable,
        }
    } else if request
        .content_type
        .is_some_and(|ct| ct.contains("application/json"))
    {
        match json_parameters(request.body) {
            Some(members) => BodyReading::Json(members),
            None if request.body.iter().all(u8::is_ascii_whitespace) => BodyReading::Nothing,
            None => BodyReading::Unreadable,
        }
    } else {
        BodyReading::Nothing
    };
    RequestReading { query, body }
}

impl RequestReading {
    /// Whether the body could not be read.
    pub fn unreadable(&self) -> bool {
        self.body == BodyReading::Unreadable
    }

    /// The form fields, when the body was a form.
    pub fn form(&self) -> Option<&StringValuesCollection> {
        match &self.body {
            BodyReading::Form(form) => Some(form),
            _ => None,
        }
    }

    /// The parameter dictionary, as [`extract_all_parameters`] builds it: query values first,
    /// then the body's, which override them.
    pub fn parameters(&self) -> Parameters {
        let mut parameters = Parameters::new();
        for (key, values) in self.query.iter() {
            parameters.insert(key.to_string(), join_values(values));
        }
        match &self.body {
            BodyReading::Form(form) => {
                for (key, values) in form.iter() {
                    parameters.insert(key.to_string(), join_values(values));
                }
            }
            BodyReading::Json(members) => {
                for (key, value) in members {
                    parameters.insert(key.clone(), value.clone());
                }
            }
            BodyReading::Nothing | BodyReading::Unreadable => {}
        }
        parameters
    }

    /// Every occurrence of a parameter from the query and then a form, blank ones skipped, as
    /// [`extract_parameter_values`] reads them.
    pub fn parameter_values(&self, name: &str) -> Vec<String> {
        let keep = |v: &&String| !dotnet::is_blank(v);
        let mut values = Vec::new();
        if let Some(query) = self.query.get(name) {
            values.extend(query.iter().filter(keep).cloned());
        }
        if let Some(body) = self.form().and_then(|form| form.get(name)) {
            values.extend(body.iter().filter(keep).cloned());
        }
        values
    }

    /// Every value of `name` in any case, from the query and the body of either kind, each
    /// trimmed, blank ones left out.
    pub fn every_value(&self, name: &str) -> Vec<String> {
        let mut values: Vec<String> = Vec::new();
        let mut take = |found: &mut dyn Iterator<Item = &String>| {
            values.extend(found.map(|v| v.trim().to_string()).filter(|v| !v.is_empty()));
        };
        if let Some(found) = self.query.get(name) {
            take(&mut found.iter());
        }
        match &self.body {
            BodyReading::Form(form) => {
                if let Some(found) = form.get(name) {
                    take(&mut found.iter());
                }
            }
            BodyReading::Json(members) => {
                take(
                    &mut members
                        .iter()
                        .filter(|(key, _)| dotnet::eq_ignore_case(key, name))
                        .map(|(_, value)| value),
                );
            }
            BodyReading::Nothing | BodyReading::Unreadable => {}
        }
        values
    }

    /// The body's values as one collection, keys matched ignoring case: a form's fields, or a
    /// JSON object's members.
    pub fn body_values(&self) -> Option<StringValuesCollection> {
        match &self.body {
            BodyReading::Form(form) => Some(form.clone()),
            BodyReading::Json(members) => {
                let mut values = StringValuesCollection::new();
                for (key, value) in members {
                    values.append(key.clone(), value.clone());
                }
                Some(values)
            }
            BodyReading::Nothing | BodyReading::Unreadable => None,
        }
    }
}

#[cfg(test)]
#[path = "subsonic_request_parser_tests.rs"]
mod tests;
