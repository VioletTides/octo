//! A live Navidrome transcode announces an estimated Content-Length (estimateContentLength=true,
//! which the Octo app sends on mobile data) and then sends a few percent less. Octo used to
//! promise the client that estimate, so the copy failed after the last real byte and the
//! connection was cut (Verified 2026-10-05 on LXC 111: 6,101,975 announced, 5,960,653 sent).
//! Navidrome here is a real socket, so the HTTP client fails exactly as it does against the real
//! one.

use axum::body::Body;
use axum::http::{Method, StatusCode};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::test_app::*;

/// Navidrome as these requests see it: a 50,000 byte song. A transcode (any format but raw) is
/// made live, so it says Accept-Ranges: none and, when asked for an estimate, announces 3% more
/// than it sends. A raw request is served from the file, ranges and all. Every answer closes the
/// connection, as Go does after a short body.
struct SocketNavidrome {
    url: String,
    serving: tokio::task::JoinHandle<()>,
}

const SIZE: usize = 50_000;
const ESTIMATE: usize = 51_500;

fn song() -> Vec<u8> {
    (0..SIZE).map(|i| (i % 251) as u8).collect()
}

impl SocketNavidrome {
    async fn start() -> SocketNavidrome {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
        let url = format!("http://{}", listener.local_addr().expect("an address"));
        let serving = tokio::spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                tokio::spawn(serve(client));
            }
        });
        SocketNavidrome { url, serving }
    }
}

impl Drop for SocketNavidrome {
    fn drop(&mut self) {
        self.serving.abort();
    }
}

async fn serve(mut stream: TcpStream) {
    let head = read_head(&mut stream).await;
    let mut lines = head.split("\r\n");
    let target = lines
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .unwrap_or("/")
        .to_string();
    let range = lines
        .find(|l| l.to_ascii_lowercase().starts_with("range:"))
        .map(|l| l[6..].trim().to_string());
    let url = reqwest::Url::parse(&format!("http://navidrome{target}")).expect("a target");
    let query = |key: &str| {
        url.query_pairs()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.into_owned())
    };

    let path = url.path();
    if !path.ends_with("/rest/stream") && !path.ends_with("/rest/download") {
        write(&mut stream, "404 Not Found", &["Content-Length: 0".into()], &[]).await;
        return;
    }

    let song = song();
    if query("format").is_some_and(|f| !f.is_empty() && f != "raw") {
        let mut headers = vec![
            "Content-Type: audio/mpeg".to_string(),
            "Accept-Ranges: none".to_string(),
        ];
        if query("estimateContentLength").as_deref() == Some("true") {
            headers.push(format!("Content-Length: {ESTIMATE}"));
        }
        if path.ends_with("/rest/download") {
            headers.push("Content-Disposition: attachment; filename=\"Daft Punk - Digital Love.mp3\"".into());
        }
        write(&mut stream, "200 OK", &headers, &song).await;
        return;
    }

    if let Some(bounds) = range.as_deref().and_then(|r| r.strip_prefix("bytes=")) {
        let (start, end) = bounds.split_once('-').unwrap_or((bounds, ""));
        let start: usize = start.parse().expect("a start");
        if start >= SIZE {
            write(
                &mut stream,
                "416 Requested Range Not Satisfiable",
                &[
                    format!("Content-Range: bytes */{SIZE}"),
                    "Content-Length: 0".into(),
                ],
                &[],
            )
            .await;
            return;
        }
        let end = if end.is_empty() {
            SIZE - 1
        } else {
            end.parse::<usize>().expect("an end").min(SIZE - 1)
        };
        let slice = &song[start..=end];
        write(
            &mut stream,
            "206 Partial Content",
            &[
                "Content-Type: audio/flac".into(),
                "Accept-Ranges: bytes".into(),
                format!("Content-Range: bytes {start}-{end}/{SIZE}"),
                format!("Content-Length: {}", slice.len()),
            ],
            slice,
        )
        .await;
        return;
    }

    write(
        &mut stream,
        "200 OK",
        &[
            "Content-Type: audio/flac".into(),
            "Accept-Ranges: bytes".into(),
            format!("Content-Length: {SIZE}"),
        ],
        &song,
    )
    .await;
}

async fn read_head(stream: &mut TcpStream) -> String {
    let mut head = Vec::new();
    let mut one = [0u8; 1];
    while stream.read(&mut one).await.unwrap_or(0) == 1 {
        head.push(one[0]);
        if head.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    String::from_utf8_lossy(&head).into_owned()
}

async fn write(stream: &mut TcpStream, status: &str, headers: &[String], body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {status}\r\n{}Connection: close\r\n\r\n",
        headers.iter().map(|h| format!("{h}\r\n")).collect::<String>()
    );
    let _ = stream.write_all(head.as_bytes()).await;
    let _ = stream.write_all(body).await;
    let _ = stream.flush().await;
}

const AUTH: &str = "u=alice&t=token&s=salt&v=1.16.1&c=test";
const TRANSCODE: &str = "format=mp3&maxBitRate=192&estimateContentLength=true";

async fn fetch(navidrome: &SocketNavidrome, url: &str, range: Option<&str>) -> Reply {
    let state = state_with(settings(&navidrome.url), |_| {});
    let headers: Vec<(&str, &str)> = range.map(|r| ("Range", r)).into_iter().collect();
    // `send` reads the body to its end; a body that fails to read fails the test there.
    send(&app(&state), Method::GET, url, &headers, Body::empty()).await
}

#[tokio::test]
async fn transcode_with_an_estimated_length_arrives_whole_and_ends_cleanly() {
    let navidrome = SocketNavidrome::start().await;

    let response = fetch(
        &navidrome,
        &format!("/rest/stream?id=library-track&{AUTH}&{TRANSCODE}"),
        None,
    )
    .await;

    assert_eq!(response.status, StatusCode::OK);
    // No promise Navidrome cannot keep: the client reads to the end of a chunked body.
    assert_eq!(response.header("content-length"), None);
    assert_eq!(response.header("accept-ranges"), Some("none"));
    assert_eq!(response.media_type().as_deref(), Some("audio/mpeg"));
    assert_eq!(response.body.as_ref(), song().as_slice());
}

#[tokio::test]
async fn transcode_with_an_estimated_length_and_a_range_still_arrives_whole() {
    // Navidrome cannot seek in a transcode it is still making, so it ignores the Range and
    // answers 200 with the whole song; Octo passes that on as it is.
    let navidrome = SocketNavidrome::start().await;

    let response = fetch(
        &navidrome,
        &format!("/rest/stream?id=library-track&{AUTH}&{TRANSCODE}"),
        Some("bytes=0-"),
    )
    .await;

    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.header("content-length"), None);
    assert_eq!(response.body.as_ref(), song().as_slice());
}

#[tokio::test]
async fn download_of_a_transcode_with_an_estimated_length_arrives_whole() {
    // rest/download goes through the general relay, which used to answer an error envelope here
    // ("Error while copying content to a stream", Verified 2026-10-05 on LXC 111).
    let navidrome = SocketNavidrome::start().await;

    let response = fetch(
        &navidrome,
        &format!("/rest/download?id=library-track&{AUTH}&{TRANSCODE}"),
        None,
    )
    .await;

    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.media_type().as_deref(), Some("audio/mpeg"));
    assert_eq!(response.body.as_ref(), song().as_slice());
    // The file keeps the name Navidrome gave it, not a nameless download.
    assert_eq!(
        response.header("content-disposition"),
        Some("attachment; filename=\"Daft Punk - Digital Love.mp3\"")
    );
}

#[tokio::test]
async fn raw_stream_keeps_its_exact_length() {
    let navidrome = SocketNavidrome::start().await;

    let response = fetch(
        &navidrome,
        &format!("/rest/stream?id=library-track&{AUTH}&estimateContentLength=true"),
        None,
    )
    .await;

    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.header("content-length"), Some(SIZE.to_string().as_str()));
    assert_eq!(response.header("accept-ranges"), Some("bytes"));
    assert_eq!(response.body.as_ref(), song().as_slice());
}

#[tokio::test]
async fn raw_stream_with_a_range_answers_206_with_the_exact_range() {
    // The iOS clients (Arpeggi, Narjo, Amperfy) probe with a Range and need a real 206.
    let navidrome = SocketNavidrome::start().await;

    let response = fetch(
        &navidrome,
        &format!("/rest/stream?id=library-track&{AUTH}"),
        Some("bytes=100-199"),
    )
    .await;

    assert_eq!(response.status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(response.header("content-length"), Some("100"));
    assert_eq!(
        response.header("content-range"),
        Some(format!("bytes 100-199/{SIZE}").as_str())
    );
    assert_eq!(response.body.as_ref(), &song()[100..200]);
}

#[tokio::test]
async fn range_past_the_end_answers_416_with_the_real_size() {
    // What a player asks after believing a too-large estimate. The real size lets it end the
    // song (ExoPlayer treats a 416 at the stated size as the end).
    let navidrome = SocketNavidrome::start().await;

    let response = fetch(
        &navidrome,
        &format!("/rest/stream?id=library-track&{AUTH}"),
        Some(&format!("bytes={SIZE}-")),
    )
    .await;

    assert_eq!(response.status, StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(
        response.header("content-range"),
        Some(format!("bytes */{SIZE}").as_str())
    );
}
