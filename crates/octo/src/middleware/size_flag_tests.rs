use axum::Router;
use axum::http::header::CONTENT_LENGTH;
use axum::middleware::from_fn_with_state;
use axum::response::IntoResponse;
use axum::routing::any;
use http_body_util::BodyExt;
use octo_core::settings::AppSettings;
use octo_subsonic::SubsonicReply;
use octo_subsonic::xml::XElement;
use tower::ServiceExt;

use super::*;

const NS: &str = "http://subsonic.org/restapi";

fn outside_answer() -> XElement {
    XElement::ns(NS, "subsonic-response").attr("status", "ok").child(
        XElement::ns(NS, "searchResult3").child(
            XElement::ns(NS, "song")
                .attr("id", "ext-1")
                .attr("title", "Emotion")
                .attr("isExternal", true),
        ),
    )
}

const OUTSIDE_JSON: &str =
    r#"{"subsonic-response":{"status":"ok","searchResult3":{"song":[{"id":"ext-1","isExternal":true}]}}}"#;

/// The middleware over a few answers: XML, JSON, audio and a compressed XML relay.
fn app(state: &AppState) -> Router {
    Router::new()
        .route(
            "/rest/search3.view",
            any(|| async { SubsonicReply::xml(&outside_answer()) }),
        )
        .route(
            "/rest/search3",
            any(|| async { SubsonicReply::json_text(OUTSIDE_JSON.to_string()) }),
        )
        .route(
            "/rest/stream",
            any(|| async { SubsonicReply::file(b"isExternal\0audio".to_vec(), "audio/mp4") }),
        )
        .route(
            "/rest/getAlbum",
            any(|| async {
                let mut response = SubsonicReply::xml(&outside_answer()).into_response();
                response
                    .headers_mut()
                    .insert(CONTENT_ENCODING, HeaderValue::from_static("gzip"));
                response
            }),
        )
        .route(
            "/api/song",
            any(|| async { SubsonicReply::xml(&outside_answer()) }),
        )
        .layer(from_fn_with_state(state.clone(), size_flag))
        .with_state(state.clone())
}

struct Answer {
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Answer {
    fn text(&self) -> String {
        String::from_utf8(self.body.clone()).expect("utf-8")
    }
}

async fn call(state: &AppState, method: Method, uri: &str, user_agent: Option<&str>) -> Answer {
    let mut request = Request::builder().method(method).uri(uri);
    if let Some(user_agent) = user_agent {
        request = request.header(USER_AGENT, user_agent);
    }
    let response = app(state)
        .oneshot(request.body(Body::empty()).expect("request"))
        .await
        .expect("infallible");
    let (parts, body) = response.into_parts();
    Answer {
        headers: parts.headers,
        body: body.collect().await.expect("body").to_bytes().to_vec(),
    }
}

fn state() -> AppState {
    AppState::for_tests(AppSettings::default())
}

fn unchanged_xml() -> String {
    outside_answer().to_xml_string()
}

#[tokio::test]
async fn an_app_named_by_its_client_name_gets_a_size_on_each_outside_song() {
    let answer = call(
        &state(),
        Method::GET,
        "/rest/search3.view?u=a&c=Amperfy&f=xml",
        None,
    )
    .await;
    let expected = XElement::ns(NS, "subsonic-response").attr("status", "ok").child(
        XElement::ns(NS, "searchResult3").child(
            XElement::ns(NS, "song")
                .attr("id", "ext-1")
                .attr("title", "Emotion")
                .attr("isExternal", true)
                .attr("size", "1"),
        ),
    );
    assert_eq!(answer.text(), expected.to_xml_string());
    assert_eq!(
        answer.headers.get(CONTENT_LENGTH).and_then(|v| v.to_str().ok()),
        Some(answer.body.len().to_string().as_str())
    );
}

#[tokio::test]
async fn an_app_named_by_its_user_agent_gets_the_sizes_too() {
    let answer = call(
        &state(),
        Method::GET,
        "/rest/search3?u=a&f=json",
        Some("Amperfy/2026.7 CFNetwork/1568.100.1 Darwin/25.0.0"),
    )
    .await;
    assert_eq!(
        answer.text(),
        r#"{"subsonic-response":{"status":"ok","searchResult3":{"song":[{"id":"ext-1","isExternal":true,"size":1}]}}}"#
    );
}

#[tokio::test]
async fn the_client_name_is_read_as_any_subsonic_parameter_is() {
    let state = state();
    // A repeated key is every value; a key matches ignoring case.
    let repeated = call(&state, Method::GET, "/rest/search3.view?c=test&c=Amperfy", None).await;
    assert!(repeated.text().contains("size=\"1\""));
    let upper = call(&state, Method::GET, "/rest/search3.view?C=Amperfy", None).await;
    assert!(upper.text().contains("size=\"1\""));
    let empty = call(&state, Method::GET, "/rest/search3.view?c=", None).await;
    assert_eq!(empty.text(), unchanged_xml());
}

#[tokio::test]
async fn every_other_app_gets_the_answer_unchanged() {
    let state = state();
    let other = call(
        &state,
        Method::GET,
        "/rest/search3.view?c=Symfonium",
        Some("okhttp/4.12"),
    )
    .await;
    assert_eq!(other.text(), unchanged_xml());
    let unnamed = call(&state, Method::GET, "/rest/search3?f=json", None).await;
    assert_eq!(unnamed.text(), OUTSIDE_JSON);
}

#[tokio::test]
async fn audio_a_compressed_relay_a_head_and_non_subsonic_paths_pass_through() {
    let state = state();
    let audio = call(&state, Method::GET, "/rest/stream?c=Amperfy&id=ext-1", None).await;
    assert_eq!(audio.body, b"isExternal\0audio");
    let compressed = call(&state, Method::GET, "/rest/getAlbum?c=Amperfy", None).await;
    assert_eq!(compressed.text(), unchanged_xml());
    let head = call(&state, Method::HEAD, "/rest/search3.view?c=Amperfy", None).await;
    assert!(!String::from_utf8_lossy(&head.body).contains("size="));
    let native = call(&state, Method::GET, "/api/song?c=Amperfy", Some("Amperfy/1")).await;
    assert_eq!(native.text(), unchanged_xml());
}

#[tokio::test]
async fn the_list_of_apps_is_read_on_every_call() {
    let state = state();
    let mut settings = AppSettings::default();
    settings.subsonic.size_flag_clients = String::new();
    state.settings.set(settings);
    let off = call(&state, Method::GET, "/rest/search3.view?c=Amperfy", None).await;
    assert_eq!(off.text(), unchanged_xml());

    let mut settings = AppSettings::default();
    settings.subsonic.size_flag_clients = "Narjo".to_string();
    state.settings.set(settings);
    let listed = call(&state, Method::GET, "/rest/search3.view?c=Narjo", None).await;
    assert!(listed.text().contains("size=\"1\""));
}

fn streamed(chunks: &[&'static str]) -> Body {
    Body::from_stream(stream::iter(
        chunks
            .iter()
            .map(|chunk| Ok::<_, std::io::Error>(Bytes::from_static(chunk.as_bytes())))
            .collect::<Vec<_>>(),
    ))
}

#[tokio::test]
async fn an_answer_within_the_limit_is_read_whole() {
    match read_whole(streamed(&["<a ", "isExternal=\"true\"", "/>"]), 64).await {
        Read::Whole(bytes) => assert_eq!(bytes.as_ref(), b"<a isExternal=\"true\"/>"),
        Read::Unread(_) => panic!("read whole"),
    }
}

#[tokio::test]
async fn an_answer_past_the_limit_with_no_length_goes_out_byte_for_byte() {
    let chunks = [
        "<subsonic-response>",
        "<song isExternal=\"true\"/>",
        "</subsonic-response>",
    ];
    let Read::Unread(body) = read_whole(streamed(&chunks), 20).await else {
        panic!("over the limit");
    };
    let sent = body.collect().await.expect("body").to_bytes();
    assert_eq!(sent.as_ref(), chunks.concat().as_bytes());
}

#[tokio::test]
async fn an_answer_that_fails_part_way_still_fails_for_the_app() {
    let failing = Body::from_stream(stream::iter(vec![
        Ok(Bytes::from_static(b"<subsonic-response>")),
        Err(std::io::Error::other("upstream reset")),
    ]));
    let Read::Unread(body) = read_whole(failing, 1024).await else {
        panic!("failed part way");
    };
    let mut frames = body.into_data_stream();
    assert_eq!(
        frames
            .next()
            .await
            .expect("the bytes read")
            .expect("data")
            .as_ref(),
        b"<subsonic-response>"
    );
    assert!(frames.next().await.expect("the failure").is_err());
}

#[tokio::test]
async fn a_long_answer_from_a_listed_app_passes_through_untouched() {
    let state = state();
    let long = format!(
        "<subsonic-response xmlns=\"{NS}\"><song id=\"ext-1\" isExternal=\"true\" title=\"{}\" /></subsonic-response>",
        "x".repeat(MAX_BODY)
    );
    let router = Router::new()
        .route(
            "/rest/search3.view",
            any(move || {
                let long = long.clone();
                async move {
                    // Streamed with no Content-Length, as a relay can be.
                    let body =
                        Body::from_stream(stream::once(
                            async move { Ok::<_, std::io::Error>(Bytes::from(long)) },
                        ));
                    let mut response = Response::new(body);
                    response
                        .headers_mut()
                        .insert(CONTENT_TYPE, HeaderValue::from_static("application/xml"));
                    response
                }
            }),
        )
        .layer(from_fn_with_state(state.clone(), size_flag))
        .with_state(state.clone());
    let response = router
        .oneshot(
            Request::builder()
                .uri("/rest/search3.view?c=Amperfy")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("infallible");
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.expect("body").to_bytes();
    assert!(body.len() > MAX_BODY);
    assert!(!String::from_utf8_lossy(&body[..120]).contains("size="));
}

#[test]
fn only_plain_xml_and_json_answers_are_read() {
    let headers = |content_type: &str| {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_str(content_type).expect("header"));
        headers
    };
    assert_eq!(
        answer_format(StatusCode::OK, &headers("application/xml")),
        Some(Format::Xml)
    );
    assert_eq!(
        answer_format(StatusCode::OK, &headers("text/xml; charset=utf-8")),
        Some(Format::Xml)
    );
    assert_eq!(
        answer_format(StatusCode::OK, &headers("application/json; charset=utf-8")),
        Some(Format::Json)
    );
    assert_eq!(answer_format(StatusCode::OK, &headers("audio/flac")), None);
    assert_eq!(answer_format(StatusCode::OK, &headers("text/javascript")), None);
    assert_eq!(
        answer_format(StatusCode::NOT_FOUND, &headers("application/xml")),
        None
    );
    assert_eq!(answer_format(StatusCode::OK, &HeaderMap::new()), None);

    let mut oversized = headers("application/xml");
    oversized.insert(CONTENT_LENGTH, HeaderValue::from(MAX_BODY + 1));
    assert_eq!(answer_format(StatusCode::OK, &oversized), None);
}
