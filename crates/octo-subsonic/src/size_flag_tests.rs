use super::*;

const NS: &str = "http://subsonic.org/restapi";

fn envelope(rows: Vec<XElement>) -> XElement {
    XElement::ns(NS, "subsonic-response")
        .attr("status", "ok")
        .attr("version", "1.16.1")
        .child(XElement::ns(NS, "searchResult3").children(rows))
}

fn outside(id: &str) -> XElement {
    XElement::ns(NS, "song")
        .attr("id", id)
        .attr("title", "Emotion")
        .attr("suffix", "m4a")
        .attr("albumId", "al-1")
        .attr("isExternal", true)
}

fn library(id: &str, size: &str) -> XElement {
    XElement::ns(NS, "song")
        .attr("id", id)
        .attr("title", "Nightcall")
        .attr("size", size)
        .attr("isExternal", false)
}

fn flagged_xml(document: &XElement) -> Option<XElement> {
    flag_sizes(document.to_xml_string().as_bytes(), Format::Xml)
        .map(|bytes| XElement::parse(std::str::from_utf8(&bytes).expect("utf-8")).expect("xml"))
}

fn sizes(document: &XElement) -> Vec<Option<String>> {
    document
        .elements()
        .flat_map(XElement::elements)
        .map(|song| song.attribute("size").map(str::to_string))
        .collect()
}

#[test]
fn an_outside_song_without_a_size_is_given_one_and_nothing_else_changes() {
    let document = envelope(vec![outside("ext-1"), library("lib-1", "24740366")]);
    let flagged = flagged_xml(&document).expect("changed");

    assert_eq!(
        sizes(&flagged),
        [Some("1".to_string()), Some("24740366".to_string())]
    );
    // The same document with only that attribute added, written the same way.
    let expected = envelope(vec![
        outside("ext-1").attr("size", "1"),
        library("lib-1", "24740366"),
    ]);
    assert_eq!(flagged.to_xml_string(), expected.to_xml_string());
}

#[test]
fn a_known_outside_size_is_kept() {
    let document = envelope(vec![outside("ext-1").attr("size", "6755585")]);
    assert_eq!(flagged_xml(&document), None);
}

#[test]
fn an_outside_size_of_zero_is_given_one() {
    let document = envelope(vec![outside("ext-1").attr("size", "0")]);
    assert_eq!(
        sizes(&flagged_xml(&document).expect("changed")),
        [Some("1".to_string())]
    );
}

#[test]
fn a_library_song_without_a_size_is_left_alone() {
    let document = envelope(vec![
        XElement::ns(NS, "song")
            .attr("id", "lib-2")
            .attr("isExternal", false),
    ]);
    assert_eq!(flag_sizes(document.to_xml_string().as_bytes(), Format::Xml), None);
}

#[test]
fn a_size_too_large_for_32_bits_is_held_to_the_largest_for_any_song() {
    let document = envelope(vec![
        library("lib-1", "3000000000"),
        outside("ext-1").attr("size", "99999999999999999999999"),
    ]);
    let flagged = flagged_xml(&document).expect("changed");
    assert_eq!(
        sizes(&flagged),
        [Some("2147483647".to_string()), Some("2147483647".to_string())]
    );
}

#[test]
fn a_folder_row_is_never_given_a_size() {
    let document = XElement::ns(NS, "subsonic-response").child(
        XElement::ns(NS, "directory").child(
            XElement::ns(NS, "child")
                .attr("id", "ext-al-1")
                .attr("isDir", true)
                .attr("isExternal", true),
        ),
    );
    assert_eq!(flag_sizes(document.to_xml_string().as_bytes(), Format::Xml), None);
}

#[test]
fn playlist_entries_and_folder_children_are_songs_too() {
    let document = XElement::ns(NS, "subsonic-response")
        .child(
            XElement::ns(NS, "playlist").child(
                XElement::ns(NS, "entry")
                    .attr("id", "ext-1")
                    .attr("isExternal", true),
            ),
        )
        .child(
            XElement::ns(NS, "directory").child(
                XElement::ns(NS, "child")
                    .attr("id", "ext-2")
                    .attr("isExternal", true),
            ),
        );
    let flagged = flagged_xml(&document).expect("changed");
    assert_eq!(sizes(&flagged), [Some("1".to_string()), Some("1".to_string())]);
}

#[test]
fn an_album_marked_outside_is_not_a_song() {
    let document = XElement::ns(NS, "subsonic-response").child(
        XElement::ns(NS, "albumList2").child(
            XElement::ns(NS, "album")
                .attr("id", "ext-al-1")
                .attr("isExternal", true),
        ),
    );
    assert_eq!(flag_sizes(document.to_xml_string().as_bytes(), Format::Xml), None);
}

#[test]
fn json_songs_under_song_entry_and_child_are_flagged_and_the_rest_is_kept_as_written() {
    let body = r#"{"subsonic-response":{"status":"ok","version":"1.16.1","searchResult3":{"song":[{"id":"ext-1","title":"isn't","duration":231,"bitRate":128,"isExternal":true},{"id":"lib-1","size":24740366,"replayGain":{"trackGain":-8.50},"isExternal":false}]},"playlist":{"entry":[{"id":"ext-2","size":0,"isExternal":true}]},"song":{"id":"ext-3","isExternal":true,"size":null}}}"#;
    let flagged = flag_sizes(body.as_bytes(), Format::Json).expect("changed");
    let flagged = String::from_utf8(flagged).expect("utf-8");

    let expected = r#"{"subsonic-response":{"status":"ok","version":"1.16.1","searchResult3":{"song":[{"id":"ext-1","title":"isn\u0027t","duration":231,"bitRate":128,"isExternal":true,"size":1},{"id":"lib-1","size":24740366,"replayGain":{"trackGain":-8.50},"isExternal":false}]},"playlist":{"entry":[{"id":"ext-2","size":1,"isExternal":true}]},"song":{"id":"ext-3","isExternal":true,"size":1}}}"#;
    assert_eq!(flagged, expected);
}

#[test]
fn json_albums_and_known_sizes_are_left_alone() {
    let body = r#"{"subsonic-response":{"album":[{"id":"ext-al-1","isExternal":true}],"song":[{"id":"ext-1","size":6755585,"isExternal":true}]}}"#;
    assert_eq!(flag_sizes(body.as_bytes(), Format::Json), None);
}

#[test]
fn json_sizes_too_large_for_32_bits_are_held_to_the_largest() {
    let body = r#"{"subsonic-response":{"song":[{"id":"lib-1","size":3000000000}]}}"#;
    let flagged = flag_sizes(body.as_bytes(), Format::Json).expect("changed");
    assert_eq!(
        String::from_utf8(flagged).expect("utf-8"),
        r#"{"subsonic-response":{"song":[{"id":"lib-1","size":2147483647}]}}"#
    );
}

#[test]
fn an_answer_with_no_outside_song_and_no_large_size_is_not_read() {
    // Not even valid XML: the byte check alone decides.
    assert_eq!(flag_sizes(b"<song size=\"123\" <<", Format::Xml), None);
    assert_eq!(flag_sizes(b"{\"song\":[{\"size\":123}", Format::Json), None);
}

#[test]
fn a_body_that_is_not_an_answer_is_left_alone() {
    assert_eq!(flag_sizes(b"<song isExternal=\"true\" <<", Format::Xml), None);
    assert_eq!(
        flag_sizes(b"{\"song\":[{\"isExternal\":true}", Format::Json),
        None
    );
    assert_eq!(flag_sizes(&[0xff, 0xfe, b'i', b's'], Format::Xml), None);
    assert_eq!(flag_sizes(b"isExternal\xff\xfe", Format::Json), None);
}

#[test]
fn a_relayed_document_with_a_declaration_still_reads() {
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><subsonic-response xmlns=\"{NS}\" status=\"ok\"><song id=\"ext-1\" isExternal=\"true\"></song></subsonic-response>"
    );
    let flagged = flag_sizes(body.as_bytes(), Format::Xml).expect("changed");
    let flagged = XElement::parse(std::str::from_utf8(&flagged).expect("utf-8")).expect("xml");
    assert_eq!(
        flagged.elements().next().and_then(|song| song.attribute("size")),
        Some("1")
    );
}
