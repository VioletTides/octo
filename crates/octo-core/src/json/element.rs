//! Strict accessors over a `serde_json::Value`, for the readers that walk an external
//! service's answer by hand.
//!
//! These fail where serde_json would quietly answer `None`: a property read on something that
//! is not an object, a string read on a number, an `i32` read on `3.5`, a required property
//! that is missing. A failure ends the whole read: the accessors return a [`Result`] and the
//! readers pass it on with `?`, so a malformed answer reads as a failed lookup rather than half
//! an answer.

use serde_json::Value;

/// Why an accessor failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ElementError {
    /// The element is not of the kind the accessor needs.
    #[error(
        "The requested operation requires an element of type '{expected}', but the target element has type '{actual}'."
    )]
    WrongKind {
        expected: &'static str,
        actual: &'static str,
    },
    /// A required property is missing.
    #[error("The given key '{0}' was not present in the dictionary.")]
    KeyNotFound(String),
    /// A number that does not fit the type asked for.
    #[error("One or more errors occurred: the JSON value could not be converted to {0}.")]
    Format(&'static str),
}

pub type ElementResult<T> = Result<T, ElementError>;

/// The element's kind, by the name the error messages use.
pub fn kind(value: &Value) -> &'static str {
    match value {
        Value::Object(_) => "Object",
        Value::Array(_) => "Array",
        Value::String(_) => "String",
        Value::Number(_) => "Number",
        Value::Bool(true) => "True",
        Value::Bool(false) => "False",
        Value::Null => "Null",
    }
}

fn wrong(expected: &'static str, value: &Value) -> ElementError {
    ElementError::WrongKind {
        expected,
        actual: kind(value),
    }
}

/// An optional property: fails unless the element is an object.
pub fn try_get_property<'a>(element: &'a Value, name: &str) -> ElementResult<Option<&'a Value>> {
    match element {
        Value::Object(map) => Ok(map.get(name)),
        other => Err(wrong("Object", other)),
    }
}

/// A required property: fails unless the element is an object that has the property.
pub fn get_property<'a>(element: &'a Value, name: &str) -> ElementResult<&'a Value> {
    try_get_property(element, name)?.ok_or_else(|| ElementError::KeyNotFound(name.to_string()))
}

/// The text of a string, `None` for a JSON null, an error for anything else.
pub fn get_string(value: &Value) -> ElementResult<Option<&str>> {
    match value {
        Value::String(text) => Ok(Some(text)),
        Value::Null => Ok(None),
        other => Err(wrong("String", other)),
    }
}

/// A number written as an integer that fits an `i32`, else `None`. Never fails for a number;
/// a non-number is an error.
pub fn try_get_int32(value: &Value) -> ElementResult<Option<i32>> {
    match value {
        Value::Number(number) => Ok(number.as_i64().and_then(|n| i32::try_from(n).ok())),
        other => Err(wrong("Number", other)),
    }
}

/// An `i32`: fails for a number that is not an integer in range, or a non-number.
pub fn get_int32(value: &Value) -> ElementResult<i32> {
    try_get_int32(value)?.ok_or(ElementError::Format("Int32"))
}

/// An `i64`: fails for a number that is not an integer in range, or a non-number.
pub fn get_int64(value: &Value) -> ElementResult<i64> {
    match value {
        Value::Number(number) => number.as_i64().ok_or(ElementError::Format("Int64")),
        other => Err(wrong("Number", other)),
    }
}

/// A number as an `f64`: fails for a non-number.
pub fn get_double(value: &Value) -> ElementResult<f64> {
    match value {
        Value::Number(number) => number.as_f64().ok_or(ElementError::Format("Double")),
        other => Err(wrong("Number", other)),
    }
}

/// A boolean: fails for anything else.
pub fn get_boolean(value: &Value) -> ElementResult<bool> {
    match value {
        Value::Bool(flag) => Ok(*flag),
        other => Err(wrong("Boolean", other)),
    }
}

/// The items of an array: fails unless the element is an array.
pub fn enumerate_array(value: &Value) -> ElementResult<&[Value]> {
    match value {
        Value::Array(items) => Ok(items),
        other => Err(wrong("Array", other)),
    }
}

/// The length of an array: fails unless the element is an array.
pub fn array_length(value: &Value) -> ElementResult<usize> {
    enumerate_array(value).map(<[Value]>::len)
}

/// The element written back compactly. serde_json does not keep the source text, so this is a
/// re-serialization.
pub fn raw_text(value: &Value) -> String {
    value.to_string()
}

/// A string property: its text when the property is a string, `None` when it is missing or of
/// another kind. Fails only when the element is not an object.
pub fn str_prop<'a>(element: &'a Value, name: &str) -> ElementResult<Option<&'a str>> {
    Ok(match try_get_property(element, name)? {
        Some(Value::String(text)) => Some(text),
        _ => None,
    })
}

/// [`str_prop`], owned.
pub fn string_prop(element: &Value, name: &str) -> ElementResult<Option<String>> {
    str_prop(element, name).map(|text| text.map(str::to_string))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn accessors_fail_on_the_wrong_kind_or_a_missing_key() {
        assert!(try_get_property(&json!("x"), "a").is_err());
        assert_eq!(try_get_property(&json!({"a": 1}), "b").unwrap(), None);
        assert!(get_property(&json!({}), "id").is_err());
        assert!(get_string(&json!(5)).is_err());
        assert_eq!(get_string(&json!(null)).unwrap(), None);
        assert_eq!(get_int32(&json!(7)).unwrap(), 7);
        assert!(get_int32(&json!(3.5)).is_err());
        assert!(get_int32(&json!(4_000_000_000_i64)).is_err());
        assert_eq!(try_get_int32(&json!(3.5)).unwrap(), None);
        assert_eq!(str_prop(&json!({"a": 1}), "a").unwrap(), None);
        assert_eq!(str_prop(&json!({"a": "x"}), "a").unwrap(), Some("x"));
    }
}
