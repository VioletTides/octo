use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use super::*;

fn jpeg(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = Vec::new();
    image::DynamicImage::new_rgb8(width, height)
        .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Jpeg)
        .expect("encoded");
    bytes
}

fn cache(dir: &Path, limit: u64) -> LocalCoverCache {
    LocalCoverCache::new(dir.to_path_buf(), Arc::new(move || limit))
}

/// A fetch that counts its calls and answers `answer`.
fn counted(calls: &Arc<AtomicUsize>, answer: Fetched) -> impl FnOnce(u32) -> std::future::Ready<Fetched> {
    let calls = Arc::clone(calls);
    move |side| {
        assert_eq!(side, MASTER_SIDE, "the kept copy is asked for at full size");
        calls.fetch_add(1, Ordering::SeqCst);
        std::future::ready(answer)
    }
}

#[tokio::test]
async fn a_kept_cover_is_read_from_disk_after_a_restart() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let calls = Arc::new(AtomicUsize::new(0));
    let picture = jpeg(400, 400);

    let first = cache(dir.path(), 1 << 30);
    let answer = first
        .cover(
            "mf-1_6700aa",
            600,
            counted(&calls, Fetched::Picture(picture.clone())),
        )
        .await
        .expect("a picture");
    assert_eq!(answer.bytes.as_ref(), &picture);
    assert_eq!(answer.content_type, "image/jpeg");

    let again = cache(dir.path(), 1 << 30);
    let answer = again
        .cover("mf-1_6700aa", 600, counted(&calls, Fetched::Other))
        .await
        .expect("a picture");
    assert_eq!(answer.bytes.as_ref(), &picture);
    assert_eq!(calls.load(Ordering::SeqCst), 1, "fetched once");
    assert_eq!(again.kept_count(), 1);
    assert_eq!(again.kept_bytes(), picture.len() as u64);
}

#[tokio::test]
async fn smaller_sizes_are_cut_from_the_kept_copy() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let calls = Arc::new(AtomicUsize::new(0));
    let cache = cache(dir.path(), 1 << 30);

    for (size, side) in [(300, 300), (150, 150), (600, 600), (1200, 1200), (300, 300)] {
        let answer = cache
            .cover(
                "al-9_aa",
                size,
                counted(&calls, Fetched::Picture(jpeg(1200, 1200))),
            )
            .await
            .expect("a picture");
        assert_eq!(cover_image::measure(&answer.bytes), Some((side, side)), "{size}");
        assert_eq!(answer.content_type, "image/jpeg");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // Larger than the kept copy, which is not the whole picture: Navidrome is asked.
    let larger = cache
        .cover("al-9_aa", 2000, counted(&calls, Fetched::Other))
        .await;
    assert_eq!(larger.err(), Some(NotKept::Ask));
}

#[tokio::test]
async fn a_copy_smaller_than_the_full_size_is_the_whole_picture() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let calls = Arc::new(AtomicUsize::new(0));
    let cache = cache(dir.path(), 1 << 30);
    let small = jpeg(500, 500);
    for size in [600, 2000] {
        let answer = cache
            .cover(
                "al-small_1",
                size,
                counted(&calls, Fetched::Picture(small.clone())),
            )
            .await
            .expect("a picture");
        assert_eq!(answer.bytes.as_ref(), &small, "{size}");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn what_is_not_a_picture_is_never_kept() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let calls = Arc::new(AtomicUsize::new(0));
    let cache = cache(dir.path(), 1 << 30);

    for (answer, expected) in [
        (Fetched::Other, NotKept::Ask),
        (Fetched::Unreachable, NotKept::Unreachable),
        (Fetched::Picture(b"<xml/>".to_vec()), NotKept::Ask),
    ] {
        let result = cache.cover("mf-2_bb", 300, counted(&calls, answer)).await;
        assert_eq!(result.err(), Some(expected));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 3, "asked again each time");
    assert_eq!(cache.kept_count(), 0);
    assert!(
        !dir.path().read_dir().is_ok_and(|mut d| d.next().is_some()),
        "nothing on disk"
    );
}

#[tokio::test]
async fn concurrent_requests_for_one_cover_share_one_fetch() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let calls = Arc::new(AtomicUsize::new(0));
    let cache = cache(dir.path(), 1 << 30);
    let picture = jpeg(1200, 1200);

    let requests = (0..8).map(|i| {
        let calls = Arc::clone(&calls);
        let picture = picture.clone();
        let cache = cache.clone();
        async move {
            cache
                .cover("mf-3_cc", if i % 2 == 0 { 300 } else { 600 }, move |_| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    async move {
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        Fetched::Picture(picture)
                    }
                })
                .await
        }
    });
    let answers = futures::future::join_all(requests).await;
    assert!(answers.iter().all(Result::is_ok));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn the_least_recently_used_covers_go_when_the_cache_is_full() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let picture = jpeg(64, 64);
    let one = picture.len() as u64;
    let cache = cache(dir.path(), one * 3 + one / 2);
    let calls = Arc::new(AtomicUsize::new(0));

    for id in ["al-a_1", "al-b_1", "al-c_1"] {
        cache
            .cover(id, 64, counted(&calls, Fetched::Picture(picture.clone())))
            .await
            .expect("kept");
    }
    // a is used again, so b is now the least recently used.
    cache
        .cover("al-a_1", 64, counted(&calls, Fetched::Other))
        .await
        .expect("kept");
    cache
        .cover("al-d_1", 64, counted(&calls, Fetched::Picture(picture.clone())))
        .await
        .expect("kept");

    assert!(cache.is_kept("al-a_1"));
    assert!(!cache.is_kept("al-b_1"));
    assert!(cache.is_kept("al-c_1"));
    assert!(cache.is_kept("al-d_1"));
    assert_eq!(cache.kept_bytes(), one * 3);
    let gone = key_of("al-b_1");
    assert!(
        !dir.path().join(&gone[..2]).join(&gone).exists(),
        "its file is removed"
    );

    // A smaller limit after a restart is met as the folder is read.
    let smaller = LocalCoverCache::new(dir.path().to_path_buf(), Arc::new(move || one));
    smaller
        .cover("al-d_1", 64, counted(&calls, Fetched::Other))
        .await
        .ok();
    assert!(smaller.kept_bytes() <= one);
}

#[tokio::test]
async fn covers_without_a_stamp_are_asked_for_again_after_their_lifetime() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let calls = Arc::new(AtomicUsize::new(0));
    let cache = LocalCoverCache::with_plain_id_lifetime(
        dir.path().to_path_buf(),
        Arc::new(|| 1 << 30),
        Duration::ZERO,
    );
    let picture = jpeg(100, 100);
    for _ in 0..2 {
        for id in ["ar-plain", "ar-stamped_19a2b3c"] {
            cache
                .cover(id, 100, counted(&calls, Fetched::Picture(picture.clone())))
                .await
                .expect("a picture");
        }
    }
    // The plain id twice, the stamped one once.
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(cache.kept_count(), 2, "the plain one replaced in place");
}

#[tokio::test]
async fn an_off_cache_asks_navidrome() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let calls = Arc::new(AtomicUsize::new(0));
    let cache = cache(dir.path(), 0);
    assert!(!cache.is_on());
    let result = cache
        .cover("al-1_a", 300, counted(&calls, Fetched::Picture(jpeg(10, 10))))
        .await;
    assert_eq!(result.err(), Some(NotKept::Ask));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn a_stamp_is_hex_after_the_last_underscore() {
    assert!(has_art_stamp("mf-4w1MGTCeHCe_67b0c1d2"));
    assert!(has_art_stamp("al-0aewBQdaMfy_0"));
    assert!(!has_art_stamp("al-0aewBQdaMfy"));
    assert!(!has_art_stamp("al-x_"));
    assert!(!has_art_stamp("_abc"));
    assert!(!has_art_stamp("al-x_notahexstamp"));
}
