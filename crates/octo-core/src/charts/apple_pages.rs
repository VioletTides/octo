//! Apple Music's web pages carry their data as JSON in
//! `<script type="application/json" id="serialized-server-data">`. Three pages are read: a
//! chart (`/{country}/new/top-charts/songs/?genreId=N`, 50 songs and the country's genre menu),
//! a room of songs (`/{country}/room/{id}`, Best New Songs or Trending Songs, up to 100) and the
//! New page (`/{country}/new`), whose song shelves name this week's rooms. Only the outline is
//! read whole; a section is parsed only when it holds songs, so the 2.3 MB New page never
//! becomes one big tree.

use serde::Deserialize;
use serde_json::Value;
use serde_json::value::RawValue;

const SCRIPT_ID: &str = "id=\"serialized-server-data\"";
const SONGS: &str = "trackLockup";
const REGION_LINK: &str = "data-testid=\"region-list-link\"";

/// One song of a chart or a room.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppleSong {
    pub rank: u32,
    pub title: String,
    /// Every artist Apple lists, main artist first.
    pub artists: Vec<String>,
    pub album: Option<String>,
    pub song_id: Option<String>,
    pub album_id: Option<String>,
    pub duration_secs: Option<i32>,
    pub explicit: bool,
    /// Apple's artwork template, `.../{w}x{h}bb.{f}`.
    pub artwork: Option<String>,
}

/// One entry of a country's genre menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChartGenre {
    pub id: String,
    pub name: String,
}

/// A page of songs: a chart or a room.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplePage {
    pub country: String,
    /// The chart's genre (`34` for every genre); `None` on a room.
    pub genre_id: Option<String>,
    pub title: String,
    pub songs: Vec<AppleSong>,
    /// The country's genre menu on a chart page, "All Genres" (34) included; empty on a room.
    pub genres: Vec<ChartGenre>,
}

/// A shelf of songs on the New page and the room it opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SongRoom {
    pub room_id: String,
    pub title: String,
}

/// The two song rooms the New page leads with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomKind {
    BestNewSongs,
    TrendingSongs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplePageError {
    /// No `serialized-server-data` script on the page.
    NoData,
    /// The script is there but is not JSON of the expected outline.
    NotJson,
    /// The outline is there but holds no songs (or, on the New page, no song shelves).
    NoSongs,
}

#[derive(Deserialize)]
struct Doc<'a> {
    #[serde(borrow)]
    data: Vec<PageShell<'a>>,
}

#[derive(Deserialize)]
struct PageShell<'a> {
    #[serde(default)]
    intent: Option<Value>,
    #[serde(borrow)]
    data: BodyShell<'a>,
}

#[derive(Deserialize)]
struct BodyShell<'a> {
    #[serde(borrow, default)]
    sections: Vec<SectionShell<'a>>,
    #[serde(default)]
    header: Option<Value>,
}

#[derive(Deserialize)]
struct SectionShell<'a> {
    #[serde(default)]
    id: Option<String>,
    #[serde(rename = "itemKind", default)]
    item_kind: Option<String>,
    #[serde(borrow, default)]
    header: Option<&'a RawValue>,
    #[serde(borrow, default)]
    items: Option<&'a RawValue>,
}

impl SectionShell<'_> {
    fn holds_songs(&self) -> bool {
        self.item_kind.as_deref() == Some(SONGS)
    }

    fn header(&self) -> Option<Value> {
        serde_json::from_str(self.header?.get()).ok()
    }
}

fn script(html: &str) -> Result<&str, ApplePageError> {
    let at = html.find(SCRIPT_ID).ok_or(ApplePageError::NoData)?;
    let open = html[at..].find('>').ok_or(ApplePageError::NoData)? + at + 1;
    let close = html[open..].find("</script>").ok_or(ApplePageError::NoData)? + open;
    Ok(&html[open..close])
}

fn doc(json: &str) -> Result<Doc<'_>, ApplePageError> {
    serde_json::from_str(json).map_err(|_| ApplePageError::NotJson)
}

/// A chart page or a room page.
pub fn parse_songs(html: &str) -> Result<ApplePage, ApplePageError> {
    let doc = doc(script(html)?)?;
    let page = doc.data.first().ok_or(ApplePageError::NoSongs)?;
    let intent = page.intent.as_ref();
    let is_chart = text(intent, "$kind").is_some_and(|k| k.starts_with("TopChart"));
    let country = text(intent, "storefront")
        .unwrap_or_default()
        .to_ascii_lowercase();
    let section = page
        .data
        .sections
        .iter()
        .find(|s| s.holds_songs())
        .ok_or(ApplePageError::NoSongs)?;
    let items: Vec<Value> = section
        .items
        .and_then(|raw| serde_json::from_str(raw.get()).ok())
        .ok_or(ApplePageError::NoSongs)?;
    let songs: Vec<AppleSong> = items
        .iter()
        .enumerate()
        .filter_map(|(i, it)| song(i, it))
        .collect();
    if songs.is_empty() {
        return Err(ApplePageError::NoSongs);
    }
    let header = section.header();
    let item = header.as_ref().and_then(|h| h.get("item"));
    let title = item
        .and_then(|h| h.pointer("/titleLink/title"))
        .or_else(|| page.data.header.as_ref().and_then(|h| h.get("title")))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    Ok(ApplePage {
        country,
        genre_id: is_chart.then(|| text(intent, "genreId").unwrap_or_else(|| "34".into())),
        title,
        songs,
        genres: if is_chart { genres(item) } else { Vec::new() },
    })
}

/// The New page's shelves of songs, in page order.
pub fn parse_song_rooms(html: &str) -> Result<Vec<SongRoom>, ApplePageError> {
    let doc = doc(script(html)?)?;
    let page = doc.data.first().ok_or(ApplePageError::NoSongs)?;
    let rooms: Vec<SongRoom> = page
        .data
        .sections
        .iter()
        .filter(|s| s.holds_songs())
        .filter_map(|s| {
            let id = s.id.as_deref()?.rsplit(" - ").next()?.trim().to_string();
            if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let title = s
                .header()
                .and_then(|h| {
                    h.pointer("/item/titleLink/title")?
                        .as_str()
                        .map(|t| t.trim().to_string())
                })
                .unwrap_or_default();
            Some(SongRoom { room_id: id, title })
        })
        .collect();
    if rooms.is_empty() {
        Err(ApplePageError::NoSongs)
    } else {
        Ok(rooms)
    }
}

/// This week's room: by its English title when the page has one (asked with `l=en-US`), by
/// position among the song shelves only when neither title is on the page.
pub fn room_for(rooms: &[SongRoom], kind: RoomKind) -> Option<&SongRoom> {
    const TITLES: [&str; 2] = ["Best New Songs", "Trending Songs"];
    let at = match kind {
        RoomKind::BestNewSongs => 0,
        RoomKind::TrendingSongs => 1,
    };
    if let Some(room) = rooms.iter().find(|r| r.title.eq_ignore_ascii_case(TITLES[at])) {
        return Some(room);
    }
    let any_known = rooms
        .iter()
        .any(|r| TITLES.iter().any(|t| r.title.eq_ignore_ascii_case(t)));
    if any_known { None } else { rooms.get(at) }
}

/// The two-letter countries in the page's country switcher (its links are
/// `<a href="/{cc}/new" data-testid="region-list-link">`), sorted, no repeats.
pub fn parse_countries(html: &str) -> Vec<String> {
    let mut out: Vec<String> = html
        .split("<a ")
        .skip(1)
        .filter_map(|chunk| {
            let tag = chunk.split('>').next()?;
            if !tag.contains(REGION_LINK) {
                return None;
            }
            let href = tag.split("href=\"/").nth(1)?;
            let code = href.get(..2)?;
            (is_country_code(code) && href[2..].starts_with('/')).then(|| code.to_string())
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

fn song(index: usize, it: &Value) -> Option<AppleSong> {
    let title = it.get("title")?.as_str()?.trim().to_string();
    let artists: Vec<String> = it
        .get("subtitleLinks")?
        .as_array()?
        .iter()
        .filter_map(|l| l.get("title")?.as_str())
        .map(|a| a.trim().to_string())
        .filter(|a| !a.is_empty())
        .collect();
    if title.is_empty() || artists.is_empty() {
        return None;
    }
    let album_link = it.pointer("/tertiaryLinks/0");
    let rank = it
        .get("rankingText")
        .and_then(Value::as_str)
        .and_then(|r| r.trim().parse().ok())
        .unwrap_or(index as u32 + 1);
    Some(AppleSong {
        rank,
        title,
        artists,
        album: album_link
            .and_then(|l| l.get("title")?.as_str())
            .map(|a| a.trim().to_string())
            .filter(|a| !a.is_empty()),
        song_id: it
            .pointer("/contentDescriptor/identifiers/storeAdamID")
            .and_then(Value::as_str)
            .map(str::to_string),
        album_id: album_link
            .and_then(|l| l.pointer("/segue/destination/contentDescriptor/identifiers/storeAdamID"))
            .and_then(Value::as_str)
            .map(str::to_string),
        duration_secs: it
            .get("duration")
            .and_then(Value::as_i64)
            .filter(|ms| *ms > 0)
            .and_then(|ms| i32::try_from((ms + 500) / 1000).ok()),
        explicit: it
            .get("showExplicitBadge")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        artwork: it
            .pointer("/artwork/dictionary/url")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

fn genres(item: Option<&Value>) -> Vec<ChartGenre> {
    let Some(buttons) = item
        .and_then(|h| h.get("accessoryButtons"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    buttons
        .iter()
        .filter_map(|b| b.get("item"))
        .filter(|i| i.get("id").and_then(Value::as_str) == Some("songs"))
        .filter_map(|i| i.get("items")?.as_array())
        .flatten()
        .filter_map(|g| {
            let name = g.get("title")?.as_str()?.trim().to_string();
            let id = g.pointer("/segue/metadata/id")?.as_str()?.to_string();
            (!name.is_empty() && !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
                .then_some(ChartGenre { id, name })
        })
        .collect()
}

fn text(v: Option<&Value>, key: &str) -> Option<String> {
    v?.get(key)?.as_str().map(str::to_string)
}

/// A country Apple named, or a setting, goes into a URL only when it is two lowercase letters.
pub fn is_country_code(value: &str) -> bool {
    value.len() == 2 && value.bytes().all(|b| b.is_ascii_lowercase())
}

/// Apple's artwork template at one size, as a JPEG.
pub fn artwork_at(template: &str, side: u32) -> String {
    let side = side.to_string();
    template
        .replace("{w}", &side)
        .replace("{h}", &side)
        .replace("{f}", "jpg")
}

#[cfg(test)]
#[path = "apple_pages_tests.rs"]
mod tests;
