//! Tests of the request parser: the parameter dictionary, and the query, form and JSON parsing
//! rules it is built on.

use super::*;

fn query(q: &str) -> RequestParts<'_> {
    RequestParts {
        query: Some(q),
        ..Default::default()
    }
}

fn with_body<'a>(q: Option<&'a str>, content_type: &'a str, body: &'a str) -> RequestParts<'a> {
    RequestParts {
        query: q,
        content_type: Some(content_type),
        content_length: Some(body.len() as u64),
        body: body.as_bytes(),
    }
}

fn pairs(parameters: &Parameters) -> Vec<(&str, &str)> {
    parameters.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect()
}

#[test]
fn extract_all_parameters_query_parameters_extracts_correctly() {
    let result = extract_all_parameters(&query("u=admin&p=password&v=1.16.0&c=testclient&f=json"));

    assert_eq!(result.len(), 5);
    assert_eq!(result["u"], "admin");
    assert_eq!(result["p"], "password");
    assert_eq!(result["v"], "1.16.0");
    assert_eq!(result["c"], "testclient");
    assert_eq!(result["f"], "json");
}

#[test]
fn extract_all_parameters_form_encoded_body_extracts_correctly() {
    let result = extract_all_parameters(&with_body(
        None,
        "application/x-www-form-urlencoded",
        "u=admin&p=password&query=test+artist&artistCount=10",
    ));

    assert_eq!(result.len(), 4);
    assert_eq!(result["u"], "admin");
    assert_eq!(result["p"], "password");
    assert_eq!(result["query"], "test artist");
    assert_eq!(result["artistCount"], "10");
}

#[test]
fn extract_all_parameters_json_body_extracts_correctly() {
    let result = extract_all_parameters(&with_body(
        None,
        "application/json",
        r#"{"u":"admin","p":"password","query":"test artist","artistCount":10}"#,
    ));

    assert_eq!(result.len(), 4);
    assert_eq!(result["u"], "admin");
    assert_eq!(result["p"], "password");
    assert_eq!(result["query"], "test artist");
    assert_eq!(result["artistCount"], "10");
}

#[test]
fn extract_all_parameters_query_and_form_body_merges_correctly() {
    let result = extract_all_parameters(&with_body(
        Some("u=admin&p=password&f=json"),
        "application/x-www-form-urlencoded",
        "query=test&artistCount=5",
    ));

    assert_eq!(result.len(), 5);
    assert_eq!(result["u"], "admin");
    assert_eq!(result["p"], "password");
    assert_eq!(result["f"], "json");
    assert_eq!(result["query"], "test");
    assert_eq!(result["artistCount"], "5");
}

#[test]
fn extract_all_parameters_empty_request_returns_empty_dictionary() {
    assert!(extract_all_parameters(&RequestParts::default()).is_empty());
}

#[test]
fn extract_all_parameters_special_characters_encodes_correctly() {
    let result = extract_all_parameters(&query("query=rock+%26+roll&artist=AC%2FDC"));

    assert_eq!(result.len(), 2);
    assert_eq!(result["query"], "rock & roll");
    assert_eq!(result["artist"], "AC/DC");
}

#[test]
fn extract_all_parameters_invalid_json_ignores_body() {
    let result = extract_all_parameters(&with_body(Some("u=admin"), "application/json", "{invalid json}"));

    assert_eq!(pairs(&result), [("u", "admin")]);
}

#[test]
fn extract_all_parameters_null_json_values_handles_gracefully() {
    let result = extract_all_parameters(&with_body(
        None,
        "application/json",
        r#"{"u":"admin","p":null,"query":"test"}"#,
    ));

    assert_eq!(result.len(), 3);
    assert_eq!(result["u"], "admin");
    assert_eq!(result["p"], "");
    assert_eq!(result["query"], "test");
}

#[test]
fn extract_all_parameters_duplicate_keys_body_overrides_query() {
    let result = extract_all_parameters(&with_body(
        Some("format=xml&query=old"),
        "application/json",
        r#"{"query":"new","artist":"Beatles"}"#,
    ));

    assert_eq!(result.len(), 3);
    assert_eq!(result["format"], "xml");
    assert_eq!(result["query"], "new"); // Body overrides query
    assert_eq!(result["artist"], "Beatles");
}

// What follows pins the parsing rules themselves, case by case.

#[test]
fn query_keys_merge_ignoring_case_under_the_first_spelling() {
    let result = extract_all_parameters(&query("u=a&U=b&f=json&F=xml"));
    assert_eq!(pairs(&result), [("u", "a,b"), ("f", "json,xml")]);
}

#[test]
fn query_segments_split_and_decode_by_the_query_rules() {
    let cases: [(&str, &[(&str, &str)]); 5] = [
        (
            "a=1&&b=2&=3&c&d=",
            &[("a", "1"), ("b", "2"), ("", "3"), ("c", ""), ("d", "")],
        ),
        (
            "q=a+b%2Bc%20d&bad=%ZZ&ff=%FF&e9=%C3%A9&lone=%&k%41=1",
            &[
                ("q", "a b+c d"),
                ("bad", "%ZZ"),
                ("ff", "%FF"),
                ("e9", "é"),
                ("lone", "%"),
                ("kA", "1"),
            ],
        ),
        // A repeated key leaves its empty values out.
        ("id=&id=B&id=", &[("id", "B")]),
        ("a=1;b=2", &[("a", "1;b=2")]),
        ("x=%C3%A9%FF%41&y=%ff", &[("x", "é%FFA"), ("y", "%ff")]),
    ];
    for (q, expected) in cases {
        assert_eq!(pairs(&extract_all_parameters(&query(q))), expected, "{q}");
    }
}

#[test]
fn the_query_collection_keeps_every_value() {
    let collection = parse_query("id=&id=B&ID=");
    assert_eq!(
        collection.get("Id"),
        Some(&["".to_string(), "B".into(), "".into()][..])
    );
    assert_eq!(join_values(collection.get("id").unwrap_or_default()), "B");
}

#[test]
fn form_fields_merge_ignoring_case_but_stay_apart_from_query_keys() {
    let result = extract_all_parameters(&with_body(
        Some("u=q"),
        "application/x-www-form-urlencoded",
        "U=f1&u=f2&p=x+y%21&k&=e&&z=%FF",
    ));
    assert_eq!(
        pairs(&result),
        [
            ("u", "q"),
            ("U", "f1,f2"),
            ("p", "x y!"),
            ("k", ""),
            ("", "e"),
            ("z", "%FF")
        ]
    );
}

#[test]
fn the_form_content_type_is_matched_exactly_ignoring_case() {
    for (content_type, is_form) in [
        ("application/x-www-form-urlencoded; charset=utf-8", true),
        ("Application/X-WWW-Form-UrlEncoded", true),
        ("application/x-www-form-urlencoded;", true),
        ("application/x-www-form-urlencodedx", false),
    ] {
        let result = extract_all_parameters(&with_body(None, content_type, "a=1"));
        let expected: &[(&str, &str)] = if is_form { &[("a", "1")] } else { &[] };
        assert_eq!(pairs(&result), expected, "{content_type}");
    }
}

#[test]
fn multipart_fields_are_read_and_files_left_out() {
    let body = "--XX\r\nContent-Disposition: form-data; name=\"a\"\r\n\r\nv1\r\n--XX\r\n\
                Content-Disposition: form-data; name=\"A\"\r\n\r\nv2\r\n--XX\r\n\
                Content-Disposition: form-data; name=\"f\"; filename=\"x.txt\"\r\nContent-Type: text/plain\r\n\r\nfile\r\n--XX--\r\n";
    let result = extract_all_parameters(&with_body(None, "multipart/form-data; boundary=XX", body));
    assert_eq!(pairs(&result), [("a", "v1,v2")]);

    let quoted = "--XX\r\nContent-Disposition: form-data; name=a\r\n\r\nv1\r\n--XX--";
    let result = extract_all_parameters(&with_body(None, "multipart/form-data; boundary=\"XX\"", quoted));
    assert_eq!(pairs(&result), [("a", "v1")]);
}

#[test]
fn a_multipart_form_that_cannot_be_read_falls_back_or_reads_nothing() {
    // No boundary: the reader failed before reading, so the body was parsed as a query string.
    let result = extract_all_parameters(&with_body(None, "multipart/form-data", "a=1&b=2"));
    assert_eq!(pairs(&result), [("a", "1"), ("b", "2")]);
    // A boundary the body never uses: the reader consumed the body, so nothing is left.
    let result = extract_all_parameters(&with_body(
        None,
        "multipart/form-data; boundary=XX",
        "garbage a=1",
    ));
    assert!(result.is_empty());
}

#[test]
fn json_values_are_read_as_their_text() {
    let result = extract_all_parameters(&with_body(
        None,
        "application/json",
        r#"{"s":"xA","n":1.50,"e":1e3,"t":true,"f":false,"z":null,"o":{ "a" : [1, 2] },"arr":[ ]}"#,
    ));
    assert_eq!(
        pairs(&result),
        [
            ("s", "xA"),
            ("n", "1.50"),
            ("e", "1e3"),
            ("t", "True"),
            ("f", "False"),
            ("z", ""),
            ("o", r#"{ "a" : [1, 2] }"#),
            ("arr", "[ ]"),
        ]
    );
    let escaped = extract_all_parameters(&with_body(None, "application/json", r#"{"a":"🎵 \"q\" \/"}"#));
    assert_eq!(pairs(&escaped), [("a", "🎵 \"q\" /")]);
}

#[test]
fn json_bodies_follow_the_json_reading_rules() {
    let deep = |levels: usize| format!("{{\"a\":{}{}}}", "[".repeat(levels), "]".repeat(levels));
    let deepest_kept = format!("{}{}", "[".repeat(63), "]".repeat(63));
    let cases: Vec<(String, &str, String, Vec<(&str, &str)>)> = vec![
        // A repeated member: the last value, in the first one's place.
        (
            "".into(),
            "application/json",
            r#"{"a":"1","b":2,"a":"3"}"#.into(),
            vec![("a", "3"), ("b", "2")],
        ),
        ("x=1".into(), "application/json", "[1,2]".into(), vec![("x", "1")]),
        // The content type check is an ordinal Contains.
        ("".into(), "Application/JSON", r#"{"a":1}"#.into(), vec![]),
        (
            "".into(),
            "text/plain; application/json",
            r#"{"a":1}"#.into(),
            vec![("a", "1")],
        ),
        (
            "".into(),
            "application/json",
            "\u{feff}{\"a\":1}".into(),
            vec![("a", "1")],
        ),
        ("".into(), "application/json", r#"{"a":1} x"#.into(), vec![]),
        (
            "".into(),
            "application/json",
            "  {\"a\":1}  \n".into(),
            vec![("a", "1")],
        ),
        ("q=1".into(), "application/json", "null".into(), vec![("q", "1")]),
        ("q=1".into(), "application/json", "".into(), vec![("q", "1")]),
        // Sixty-four levels with the object itself, and no more.
        (
            "".into(),
            "application/json",
            deep(63),
            vec![("a", deepest_kept.as_str())],
        ),
        ("".into(), "application/json", deep(64), vec![]),
        ("".into(), "application/json", deep(70), vec![]),
        // The body overrides a query value in the query value's place.
        (
            "b=q&a=q".into(),
            "application/json",
            r#"{"a":"j","c":"j"}"#.into(),
            vec![("b", "q"), ("a", "j"), ("c", "j")],
        ),
        // The dictionary is ordinal, so a key in another case is another key.
        (
            "A=q".into(),
            "application/json",
            r#"{"a":"j"}"#.into(),
            vec![("A", "q"), ("a", "j")],
        ),
    ];
    for (q, content_type, body, expected) in cases {
        let result = extract_all_parameters(&with_body(Some(&q), content_type, &body));
        assert_eq!(pairs(&result), expected, "{content_type} {body}");
    }
}

#[test]
fn a_content_type_with_no_body_reads_only_the_query() {
    let result = extract_all_parameters(&RequestParts {
        query: Some("a=1"),
        content_type: Some("text/plain"),
        content_length: None,
        body: b"",
    });
    assert_eq!(pairs(&result), [("a", "1")]);
}

#[test]
fn extract_parameter_values_keeps_repeats_apart_and_skips_blanks() {
    let request = with_body(
        Some("id=A&ID=%20&id=B"),
        "application/x-www-form-urlencoded",
        "Id=C&id=&time=1",
    );
    assert_eq!(extract_parameter_values(&request, "id"), ["A", "B", "C"]);
    assert_eq!(extract_parameter_values(&request, "time"), ["1"]);
    assert!(extract_parameter_values(&request, "submission").is_empty());
}

#[test]
fn escape_data_string_encodes_all_but_unreserved_characters() {
    assert_eq!(
        escape_data_string("a b+c/é~-_.!*'()"),
        "a%20b%2Bc%2F%C3%A9~-_.%21%2A%27%28%29"
    );
}

#[test]
fn a_readable_body_reads_to_the_same_parameters_once() {
    let cases = [
        query("u=a&U=b&id=1&id=&f=json"),
        with_body(
            Some("id=Q&f=json"),
            "application/x-www-form-urlencoded",
            "u=x&id=1&id=2",
        ),
        with_body(
            Some("u=a"),
            "application/json",
            r#"{"id":"9","n":[1, 2],"t":true}"#,
        ),
        with_body(Some("u=a"), "application/json", "\u{feff}{\"id\":\"9\"}"),
        with_body(
            None,
            "multipart/form-data; boundary=XX",
            "--XX\r\nContent-Disposition: form-data; name=\"id\"\r\n\r\n5\r\n--XX--\r\n",
        ),
        with_body(Some("u=a"), "text/plain", "u=b"),
        with_body(Some("u=a"), "application/json", "  "),
        with_body(Some("u=a"), "multipart/form-data", ""),
    ];
    for parts in cases {
        let reading = read_request(&parts);
        assert!(!reading.unreadable(), "{parts:?}");
        assert_eq!(reading.parameters(), extract_all_parameters(&parts), "{parts:?}");
        for name in ["id", "u"] {
            assert_eq!(
                reading.parameter_values(name),
                extract_parameter_values(&parts, name),
                "{parts:?}"
            );
        }
    }
}

#[test]
fn a_body_the_parameters_would_guess_at_is_unreadable() {
    for parts in [
        // No boundary, or one too long: the parameters once read the body as a query.
        with_body(None, "multipart/form-data", "u=guest&p=pw"),
        with_body(
            None,
            &format!("multipart/form-data; boundary={}", "b".repeat(80)),
            "u=guest",
        ),
        // A boundary the body never uses.
        with_body(None, "multipart/form-data; boundary=XX", "garbage u=guest"),
        // JSON that is not one object.
        with_body(None, "application/json", r#"["u","guest"]"#),
        with_body(None, "application/json", "null"),
        with_body(None, "application/json", "{ broken"),
    ] {
        let reading = read_request(&parts);
        assert!(reading.unreadable(), "{parts:?}");
        assert_eq!(
            reading.parameters(),
            extract_all_parameters(&query(parts.query.unwrap_or("")))
        );
    }
}

#[test]
fn every_value_is_read_from_the_query_and_either_kind_of_body() {
    let reading = read_request(&with_body(
        Some("id=a&ID=b"),
        "application/x-www-form-urlencoded",
        "iD=c&other=x",
    ));
    assert_eq!(reading.every_value("id"), ["a", "b", "c"]);
    let reading = read_request(&with_body(
        Some("id=a"),
        "application/json",
        r#"{"Id":" d ","x":"y"}"#,
    ));
    assert_eq!(reading.every_value("id"), ["a", "d"]);
    let values = reading.body_values().expect("a body");
    assert_eq!(values.get("id"), Some(&[" d ".to_string()][..]));
}

#[test]
fn json_object_reads_as_the_parameters_read() {
    assert!(json_object("\u{feff}{\"a\":1}".as_bytes()).is_some());
    assert!(json_object(b"{\"a\":\"\xff\"}").is_some());
    assert!(json_object(b"[1]").is_none());
    assert!(json_object(b"").is_none());
    let deep = format!("{{\"a\":{}{}}}", "[".repeat(64), "]".repeat(64));
    assert!(json_object(deep.as_bytes()).is_none());
}
