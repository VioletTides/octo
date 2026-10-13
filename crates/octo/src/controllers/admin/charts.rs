//! `GET /api/admin/charts`: what the dashboard's Charts card shows: the chart country (set,
//! detected, used), Apple Music's countries, the lists that can be turned on (Best New Songs,
//! Trending Songs, the country's genres), and how each list that is on was last fetched. Behind
//! the sign-in gate like every admin route.

use axum::extract::State;
use axum::response::Response;
use octo_core::charts::chart_settings::{self, OVERALL, genre_name};
use serde_json::{Value, json};

use super::plumbing::ok;
use crate::app::AppState;
use crate::services::common::TopSongsService;

pub async fn get_charts(State(state): State<AppState>) -> Response {
    let top_songs = &state.top_songs;
    let lists = state.settings.current().generated_playlists.clone();
    let detected = top_songs.apple().known_country();
    let (used, menu) = top_songs.menu().await;
    let on = lists.chart_ids();

    let mut choices: Vec<Value> = [chart_settings::NEW_SONGS, chart_settings::TRENDING]
        .iter()
        .map(|id| {
            let name = TopSongsService::names(&menu, id)
                .map(|(name, _)| name)
                .unwrap_or_default();
            json!({ "id": id, "name": name, "on": on.iter().any(|c| c == id) })
        })
        .collect();
    choices.extend(menu.iter().filter(|g| g.id != OVERALL).map(|g| {
        json!({
            "id": g.id,
            "name": genre_name(&menu, &g.id).unwrap_or_else(|| g.name.clone()),
            "on": on.contains(&g.id),
        })
    }));

    let fetches = top_songs.fetches();
    let listed: Vec<Value> = state
        .chart_playlists
        .lists_on()
        .iter()
        .map(|chart| {
            let name = TopSongsService::names(&menu, chart).map(|(name, _)| name);
            let in_country = name.is_some();
            let fetch = fetches.iter().find(|f| &f.chart == chart);
            json!({
                "chart": chart,
                "name": name.unwrap_or_else(|| format!("Genre {chart}")),
                "on": true,
                "inCountry": in_country,
                "source": fetch.map(|f| f.source.clone()),
                "songs": fetch.map(|f| f.songs),
                "fetchedUtc": fetch.map(|f| f.fetched_utc.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
                "error": fetch.and_then(|f| f.error.clone()),
                "roomId": fetch.and_then(|f| f.room_id.clone()),
            })
        })
        .collect();

    ok(&json!({
        "country": {
            "setting": lists.chart_country,
            "used": used,
            "detected": detected,
        },
        "countries": top_songs.apple().countries(),
        "popularNow": lists.popular_now,
        "choices": choices,
        "lists": listed,
    }))
}

#[cfg(test)]
#[path = "charts_tests.rs"]
mod tests;
