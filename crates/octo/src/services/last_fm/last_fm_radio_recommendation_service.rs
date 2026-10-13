//! Builds canonical station snapshots from bounded listening signals, asking Last.fm for the
//! candidates. The weighing, the draw and the shaping are
//! `octo_core::last_fm::last_fm_radio_recommendation_service`.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use chrono::{TimeDelta, Utc};
use indexmap::IndexMap;
use octo_core::common::dotnet;
use octo_core::last_fm::SimilarTrack;
use octo_core::last_fm::last_fm_radio_recommendation_service::{
    ARTIST_RADIO_NEIGHBOURS, Candidate, FRONTIER_ARTIST_CAP, FRONTIER_ARTISTS, FRONTIER_FEWEST_SONGS,
    FRONTIER_NEIGHBOURS, FRONTIER_SEEDS, FRONTIER_TRACKS_EACH, FrontierScores, HEARTED_SEED_AFFINITY,
    IgnoreCaseKeys, NEIGHBOUR_ARTIST_AFFINITY, Randomizer, SECOND_STEP_SEEDS, SECOND_STEP_WEIGHT,
    SEED_RECENCY_DECAY, SEEDS_AT_ONCE, artist_cap, artist_key, create, default_randomizer,
    definition_version, discount_known, key, known_artists, ranked, score_artists, score_local_tags,
    seed_score, shape, shape_with_reserve, source_weight, suppress_station_overlap, title, to_candidate,
    unknown_neighbours_first,
};
use octo_core::last_fm::last_fm_radio_seed_normalizer as seed_normalizer;
use octo_core::models::domain::Song;
use octo_core::models::radio::{LastFmRadioPlay, LastFmRadioStation, LastFmRadioStationKind};
use octo_core::radio::RadioSeed;
use octo_core::radio::radio_blend;
use octo_core::radio::radio_provider::LAST_FM;
use octo_core::settings::SettingsStore;
use parking_lot::Mutex;
use tokio::sync::Semaphore;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::info;

use super::last_fm_radio_state_store::LastFmRadioStateStore;
use super::{LastFmService, OperationCanceled};
use crate::services::radio::RadioSourceSet;

/// Provider expansion has a hard fan-out and deadline. Partial results are useful.
const PROVIDER_BUDGET: Duration = Duration::from_secs(25);

/// The similar-artist walk of Discovery Mix has this long of its own, within the build's budget.
const DISCOVERY_WALK_BUDGET: Duration = Duration::from_secs(8);

/// The caller's token with a 25 s deadline on top: either ends the build.
struct Budget<'a> {
    caller: &'a CancellationToken,
    deadline: Instant,
}

impl<'a> Budget<'a> {
    /// This budget cut to at most `limit` from now: the same caller, the earlier deadline.
    fn within(&self, limit: Duration) -> Budget<'a> {
        Budget {
            caller: self.caller,
            deadline: self.deadline.min(Instant::now() + limit),
        }
    }

    /// The call, or [`OperationCanceled`] once the caller cancels or the deadline passes. (A
    /// canceled call is dropped, which cancels its request.)
    async fn run<F: Future>(&self, call: F) -> Result<F::Output, OperationCanceled> {
        tokio::select! {
            biased;
            () = self.caller.cancelled() => Err(OperationCanceled),
            () = tokio::time::sleep_until(self.deadline) => Err(OperationCanceled),
            output = call => Ok(output),
        }
    }
}

/// Builds canonical station snapshots from bounded listening signals. Which candidates make the
/// cut is a weighted draw rather than a fixed top-N, so two refreshes of the same profile give
/// two different stations; the previous snapshot is demoted so a refresh rotates the station
/// instead of restating it.
pub struct LastFmRadioRecommendationService {
    last_fm: Arc<LastFmService>,
    state: Arc<LastFmRadioStateStore>,
    /// The Last.fm settings.
    settings: Arc<SettingsStore>,
    /// Source of the draw. Tests replace it with a seeded one so a build repeats exactly.
    randomizer: Mutex<Randomizer>,
    /// Every radio source; None keeps stations on Last.fm alone.
    sources: Option<Arc<RadioSourceSet>>,
}

impl LastFmRadioRecommendationService {
    pub fn new(
        last_fm: Arc<LastFmService>,
        state: Arc<LastFmRadioStateStore>,
        settings: Arc<SettingsStore>,
        sources: Option<Arc<RadioSourceSet>>,
    ) -> Self {
        LastFmRadioRecommendationService {
            last_fm,
            state,
            settings,
            randomizer: Mutex::new(default_randomizer()),
            sources,
        }
    }

    /// `Randomizer = ...`.
    pub fn set_randomizer(&self, randomizer: Randomizer) {
        *self.randomizer.lock() = randomizer;
    }

    /// The listener's stations, built now. An error is an answer Last.fm's reader refused, or
    /// [`OperationCanceled`] when the caller canceled or the budget ran out while an artist
    /// station was being built.
    pub async fn build(
        &self,
        username: &str,
        cancellation_token: &CancellationToken,
    ) -> anyhow::Result<Vec<LastFmRadioStation>> {
        let snapshot = self.settings.current();
        let settings = &snapshot.last_fm;
        if !settings.enable_radio {
            return Ok(Vec::new());
        }
        let user = self.state.get_user(username);
        let now = Utc::now();
        let randomizer = self.randomizer.lock().clone();
        let mut random = randomizer();
        // Grouped by key ignoring case: the first station with a key names its snapshot.
        let mut previous_snapshots: Vec<(String, IgnoreCaseKeys)> = Vec::new();
        for station in &user.stations {
            if !previous_snapshots
                .iter()
                .any(|(key, _)| dotnet::eq_ignore_case(key, &station.key))
            {
                let keys = station
                    .tracks
                    .iter()
                    .map(|track| seed_normalizer::track_key(&track.artist, &track.title))
                    .collect();
                previous_snapshots.push((station.key.clone(), keys));
            }
        }
        let empty = IgnoreCaseKeys::new();
        let previous = |station_key: &str| previous_snapshot(&previous_snapshots, &empty, station_key);
        let mut plays = user.plays.clone();
        plays.sort_by_key(|a| std::cmp::Reverse(a.played_at_utc));
        let unavailable: IgnoreCaseKeys = user
            .unavailable_tracks
            .iter()
            .filter(|track| track.retry_after_utc > Utc::now())
            .map(|track| track.key.as_str())
            .collect();
        let banned: IgnoreCaseKeys = user.radio_bans.iter().map(|ban| ban.key.as_str()).collect();
        let disliked: IgnoreCaseKeys = user
            .radio_bans
            .iter()
            .map(|ban| artist_key(&ban.artist))
            .collect();
        let known = known_artists(&plays);
        let refill_headroom = (unavailable.len() as i32).min(settings.effective_radio_track_count());
        let candidate_target = 100.min(settings.effective_radio_track_count() + refill_headroom + 10);
        let artist_scores = score_artists(&plays, now);
        // Seeds are the strongest recent signals, not simply the newest plays: a heart outranks
        // a play, a chosen play outranks one the radio served, and age decays.
        let mut by_seed_score: Vec<&LastFmRadioPlay> = plays.iter().collect();
        by_seed_score.sort_by(|a, b| {
            seed_score(b, now)
                .partial_cmp(&seed_score(a, now))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut seen_seeds = std::collections::HashSet::new();
        let track_seeds: Vec<LastFmRadioPlay> = by_seed_score
            .into_iter()
            .filter(|play| seen_seeds.insert(seed_normalizer::track_key(&play.artist, &play.title)))
            .take(8)
            .cloned()
            .collect();
        let mut tags = score_local_tags(&plays);

        // Provider expansion has a hard fan-out and deadline. Partial results are useful.
        let budget = Budget {
            caller: cancellation_token,
            deadline: Instant::now() + PROVIDER_BUDGET,
        };
        for (artist, _) in artist_scores.iter().take(5) {
            match budget.run(self.last_fm.get_artist_top_tags(artist, 6)).await {
                Ok(top_tags) => {
                    for tag in top_tags? {
                        tags.add(&tag, 1.0);
                    }
                }
                Err(canceled) if cancellation_token.is_cancelled() => return Err(canceled.into()),
                Err(_) => break,
            }
        }

        let mut stations: Vec<LastFmRadioStation> = Vec::new();
        if settings.enable_personalized_stations {
            let learned: f64 = plays
                .iter()
                .filter(|play| play.learned_signal)
                .map(source_weight)
                .sum();
            let learned = learned >= f64::from(settings.effective_minimum_plays());
            let mix_key = if learned { "your-mix" } else { "starter" };
            if settings.enable_your_mix {
                let mut mix_candidates = self
                    .tracks_from_seeds(username, &track_seeds[..track_seeds.len().min(6)], 12, &budget)
                    .await;
                let familiar: Vec<Candidate> = plays
                    .iter()
                    .enumerate()
                    .map(|(rank, play)| to_candidate(play, rank))
                    .collect();
                // The familiar share of the mix is a quota the walk enforces, so both halves are
                // drawn over their whole pools rather than the top of each list.
                let familiar_quota = if mix_candidates.is_empty() {
                    None
                } else {
                    let count = settings.effective_radio_track_count();
                    Some(
                        count
                            - dotnet::round(
                                f64::from(count) * f64::from(settings.effective_discovery_percent()) / 100.0,
                                0,
                            ) as i32,
                    )
                };
                if mix_candidates.is_empty() {
                    mix_candidates.extend(familiar.iter().cloned());
                }
                let seeds: Vec<&str> = track_seeds.iter().map(|seed| seed.artist.as_str()).collect();
                let tracks = shape(
                    familiar.into_iter().chain(mix_candidates).collect(),
                    &plays,
                    settings,
                    &unavailable,
                    &mut *random,
                    previous(mix_key),
                    artist_cap(settings, LastFmRadioStationKind::YourMix),
                    false,
                    familiar_quota,
                    Some(&banned),
                    Some(&disliked),
                );
                stations.push(create(
                    username,
                    mix_key,
                    if learned { "Your Mix" } else { "Starter Radio" },
                    if learned {
                        LastFmRadioStationKind::YourMix
                    } else {
                        LastFmRadioStationKind::Starter
                    },
                    true,
                    &seeds,
                    tracks,
                    1,
                    Utc::now(),
                ));
            }

            if learned {
                let top_tags = tags.top_by_score_then_name(3);
                // The artist and genre stations' candidates are asked for first, so a slow walk
                // for Discovery Mix never spends their time; the stations are still drawn and
                // listed in the same order as before.
                let mut artist_candidates: Vec<(String, Vec<Candidate>)> = Vec::new();
                for (artist, _) in artist_scores
                    .iter()
                    .take(settings.effective_artist_station_count().max(0) as usize)
                {
                    let candidates = self
                        .tracks_from_artist(username, artist, candidate_target, &known, &budget)
                        .await?;
                    artist_candidates.push((artist.clone(), candidates));
                }
                let mut genre_candidates: Vec<(String, Vec<Candidate>)> = Vec::new();
                for tag in tags.top_by_score(settings.effective_genre_station_count().max(0) as usize) {
                    // A genre station is for finding: the listener's own artists keep a third of
                    // their weight, so the genre's other names lead.
                    let candidates = discount_known(
                        self.tracks_from_tags(
                            username,
                            std::slice::from_ref(&tag),
                            candidate_target,
                            &budget,
                        )
                        .await?,
                        &known,
                    );
                    genre_candidates.push((tag, candidates));
                }

                if settings.enable_discovery_mix {
                    // New artists first, found through the ones the listener plays, within the
                    // walk's own few seconds; the tag charts only when the walk finds too few
                    // songs.
                    let walk_budget = budget.within(DISCOVERY_WALK_BUDGET);
                    let mut discovery = self
                        .tracks_from_frontier(&artist_scores, &known, &disliked, &walk_budget)
                        .await;
                    let mut discovery_cap = FRONTIER_ARTIST_CAP;
                    if discovery.len() < FRONTIER_FEWEST_SONGS && !top_tags.is_empty() {
                        discovery = self
                            .tracks_from_tags(username, &top_tags, candidate_target, &budget)
                            .await?;
                        discovery_cap = artist_cap(settings, LastFmRadioStationKind::Discovery);
                    }
                    if discovery.len() >= 5 {
                        // The reserve stands in for library songs past the Discovery balance's
                        // share, which only resolving the tracks can tell.
                        let shaped = shape_with_reserve(
                            discovery,
                            &plays,
                            settings,
                            &unavailable,
                            &mut *random,
                            previous("discovery"),
                            discovery_cap,
                            true,
                            Some(&banned),
                            Some(&disliked),
                        );
                        let mut station = create(
                            username,
                            "discovery",
                            "Discovery Mix",
                            LastFmRadioStationKind::Discovery,
                            true,
                            &top_tags,
                            shaped.tracks,
                            1,
                            Utc::now(),
                        );
                        station.reserve = shaped.reserve;
                        stations.push(station);
                    }
                }

                for (artist, candidates) in artist_candidates {
                    let station_key = format!("artist-{}", key(&artist));
                    if candidates.len() >= 5 {
                        let tracks = shape(
                            candidates,
                            &plays,
                            settings,
                            &unavailable,
                            &mut *random,
                            previous(&station_key),
                            artist_cap(settings, LastFmRadioStationKind::Artist),
                            false,
                            None,
                            Some(&banned),
                            Some(&disliked),
                        );
                        stations.push(create(
                            username,
                            &station_key,
                            &format!("{artist} Radio"),
                            LastFmRadioStationKind::Artist,
                            true,
                            &[artist.as_str()],
                            tracks,
                            1,
                            Utc::now(),
                        ));
                    }
                }

                for (tag, candidates) in genre_candidates {
                    let station_key = format!("genre-{}", key(&tag));
                    let artists: IgnoreCaseKeys =
                        candidates.iter().map(|item| item.track.artist.as_str()).collect();
                    if artists.len() >= 4 {
                        let tracks = shape(
                            candidates,
                            &plays,
                            settings,
                            &unavailable,
                            &mut *random,
                            previous(&station_key),
                            artist_cap(settings, LastFmRadioStationKind::Genre),
                            false,
                            None,
                            Some(&banned),
                            Some(&disliked),
                        );
                        stations.push(create(
                            username,
                            &station_key,
                            &format!("{} Radio", title(&tag)),
                            LastFmRadioStationKind::Genre,
                            true,
                            &[tag.as_str()],
                            tracks,
                            1,
                            Utc::now(),
                        ));
                    }
                }
            }
        }

        if settings.enable_discovery_stations {
            for definition in settings
                .effective_discovery_stations()
                .into_iter()
                .filter(|item| item.enabled)
            {
                let mut candidates = self
                    .tracks_from_tags(username, &definition.tags, candidate_target, &budget)
                    .await?;
                if candidates.is_empty() {
                    candidates.extend(
                        plays
                            .iter()
                            .filter(|play| {
                                definition.tags.iter().any(|tag| {
                                    contains_ignore_case(play.genre.as_deref().unwrap_or_default(), tag)
                                })
                            })
                            .enumerate()
                            .map(|(rank, play)| to_candidate(play, rank)),
                    );
                }
                let station_key = format!("pinned-{}", definition.id);
                if !candidates.is_empty() {
                    let tracks = shape(
                        candidates,
                        &plays,
                        settings,
                        &unavailable,
                        &mut *random,
                        previous(&station_key),
                        artist_cap(settings, LastFmRadioStationKind::Pinned),
                        false,
                        None,
                        Some(&banned),
                        Some(&disliked),
                    );
                    stations.push(create(
                        username,
                        &station_key,
                        &definition.name,
                        LastFmRadioStationKind::Pinned,
                        false,
                        &definition.tags,
                        tracks,
                        definition_version(&definition),
                        Utc::now(),
                    ));
                }
            }
        }

        suppress_station_overlap(&mut stations);
        for station in &mut stations {
            station.valid_until_utc = station.changed_utc
                + TimeDelta::hours(i64::from(settings.effective_refresh_interval_hours()));
        }
        info!("Built {} Last.fm radio stations for {username}", stations.len());
        Ok(stations
            .into_iter()
            .filter(|station| !station.tracks.is_empty())
            .collect())
    }

    /// Tracks similar to each seed, the seed's own weight riding along: the newest seed leads,
    /// each older one contributes [`SEED_RECENCY_DECAY`] of the previous, and a hearted seed
    /// counts half again. With Last.fm alone, its match score stays the per-track signal; with
    /// several sources, each source's list is ranked by position and weighted by its provider
    /// (see [`RadioSourceSet::weights`]), and seeds are asked [`SEEDS_AT_ONCE`] at a time so a
    /// slow source costs one timeout per three seeds.
    async fn tracks_from_seeds(
        &self,
        username: &str,
        seeds: &[LastFmRadioPlay],
        each: i32,
        budget: &Budget<'_>,
    ) -> Vec<Candidate> {
        let ordered: Vec<(&LastFmRadioPlay, f64)> = seeds
            .iter()
            .enumerate()
            .map(|(position, seed)| {
                (
                    seed,
                    SEED_RECENCY_DECAY.powi(position as i32)
                        * if seed.hearted { HEARTED_SEED_AFFINITY } else { 1.0 },
                )
            })
            .collect();
        let Some(sources) = &self.sources else {
            let mut result = Vec::new();
            for (seed, affinity) in ordered {
                let source = seed_normalizer::track_key(&seed.artist, &seed.title);
                match budget
                    .run(self.last_fm.get_similar_tracks(&seed.artist, &seed.title, each))
                    .await
                {
                    Ok(tracks) => result.extend(ranked(tracks, &source, affinity)),
                    Err(_) => break,
                }
            }
            return result;
        };

        let weights = sources.weights(Some(username), false);
        let gate = Semaphore::new(SEEDS_AT_ONCE);
        let empty = IndexMap::new();
        let lists = futures::future::join_all(ordered.into_iter().map(|(seed, affinity)| {
            let (gate, weights, empty) = (&gate, &weights, &empty);
            async move {
                let Ok(_permit) = budget.run(gate.acquire()).await else {
                    return Vec::new();
                };
                let source = seed_normalizer::track_key(&seed.artist, &seed.title);
                let library_song = (seed.is_local && !seed.song_id.is_empty()).then(|| Song {
                    id: seed.song_id.clone(),
                    title: seed.title.clone(),
                    artist: seed.artist.clone(),
                    is_local: true,
                    ..Default::default()
                });
                let radio_seed = RadioSeed::new(
                    seed.artist.clone(),
                    seed.title.clone(),
                    seed.duration,
                    library_song,
                );
                let Ok(answers) = budget.run(sources.ask_all(&radio_seed, each, empty, None)).await else {
                    return Vec::new();
                };
                let mut found = Vec::new();
                for answer in answers {
                    let factor = radio_blend::station_factor(answer.r#match)
                        * radio_blend::thin(&answer, each)
                        * weights.get(&answer.provider).copied().unwrap_or(1.0);
                    if factor <= 0.0 {
                        continue;
                    }
                    // Sources score on different scales (Last.fm's match, a flat 1 from YouTube
                    // Music), so with several sources each list is ranked by position alone, as
                    // song radio does, and the provider weights say how much each list counts.
                    let tracks: Vec<SimilarTrack> = answer
                        .library_songs
                        .iter()
                        .map(|song| {
                            SimilarTrack::new(song.artist.clone(), song.title.clone(), 1.0)
                                .with_duration(song.duration)
                                .with_provider(Some(answer.provider.clone()))
                        })
                        .chain(answer.tracks.iter().map(|track| SimilarTrack {
                            r#match: 1.0,
                            ..track.clone()
                        }))
                        .collect();
                    found.extend(ranked(
                        tracks,
                        &format!("{}:{source}", answer.provider),
                        affinity * factor,
                    ));
                }
                found
            }
        }))
        .await;
        lists.into_iter().flatten().collect()
    }

    async fn tracks_from_tags(
        &self,
        username: &str,
        tags: &[String],
        each: i32,
        budget: &Budget<'_>,
    ) -> anyhow::Result<Vec<Candidate>> {
        let mut result = Vec::new();
        for tag in tags.iter().take(5) {
            match budget.run(self.last_fm.get_tag_top_tracks(tag, each)).await {
                Ok(tracks) => result.extend(ranked(tracks?, &format!("tag:{tag}"), 1.0)),
                Err(_) => break,
            }
            // Last.fm's tag chart is the line above; the other sources' answers for the tag (LB
            // Radio, with a ListenBrainz token) join it, each list weighted by its provider.
            if let Some(sources) = &self.sources {
                let weights = sources.weights(Some(username), false);
                match budget.run(sources.tag_all(tag, each)).await {
                    Ok(answers) => {
                        for answer in answers.into_iter().filter(|answer| answer.provider != LAST_FM) {
                            let weight = weights.get(&answer.provider).copied().unwrap_or(1.0);
                            let source = format!("{}:tag:{tag}", answer.provider);
                            result.extend(ranked(answer.tracks, &source, weight));
                        }
                    }
                    Err(_) => break,
                }
            }
        }
        Ok(result)
    }

    /// The seed artist's own top tracks lead; similar artists' top tracks ride at
    /// [`NEIGHBOUR_ARTIST_AFFINITY`] so the station stays about who it is named for. Of
    /// [`ARTIST_RADIO_NEIGHBOURS`] neighbours, the five used are the strongest with the ones the
    /// listener knows counted at a third of their match: a small artist whose closest neighbours
    /// are all familiar still leads somewhere new.
    async fn tracks_from_artist(
        &self,
        username: &str,
        artist: &str,
        candidate_target: i32,
        known: &IgnoreCaseKeys,
        budget: &Budget<'_>,
    ) -> anyhow::Result<Vec<Candidate>> {
        let mut result = Vec::new();
        // The build's budget running out keeps what was found so far: the artist's own top
        // tracks alone still make a station.
        let Ok(own) = budget
            .run(
                self.last_fm
                    .get_artist_top_tracks(artist, 50.min(candidate_target)),
            )
            .await
        else {
            return Ok(result);
        };
        result.extend(ranked(own, &format!("artist:{artist}"), 1.0));
        let Ok(similar) = budget
            .run(self.last_fm.get_similar_artists(artist, ARTIST_RADIO_NEIGHBOURS))
            .await
        else {
            return Ok(result);
        };
        for similar in unknown_neighbours_first(similar?, known).into_iter().take(5) {
            let Ok(tracks) = budget
                .run(
                    self.last_fm
                        .get_artist_top_tracks(&similar.name, 20.min(6.max(candidate_target / 5))),
                )
                .await
            else {
                return Ok(result);
            };
            result.extend(ranked(
                tracks,
                &format!("artist:{}", similar.name),
                NEIGHBOUR_ARTIST_AFFINITY,
            ));
        }
        // The other sources' radio for the artist (YouTube Music's, LB Radio's), beside the
        // similar artists' songs. Last.fm is not asked again: its answer is the lines above.
        if let Some(sources) = &self.sources {
            let weights = sources.weights(Some(username), false);
            let seed = RadioSeed::new(artist, "", None, None);
            let empty = IndexMap::new();
            let not_last_fm =
                |source: &dyn crate::services::radio::IRadioSource| source.provider() != LAST_FM;
            let Ok(answers) = budget
                .run(sources.ask_all(&seed, candidate_target, &empty, Some(&not_last_fm)))
                .await
            else {
                return Ok(result);
            };
            for answer in answers.into_iter().filter(|answer| answer.provider != LAST_FM) {
                let weight = weights.get(&answer.provider).copied().unwrap_or(1.0);
                let source = format!("{}:artist:{artist}", answer.provider);
                result.extend(ranked(answer.tracks, &source, NEIGHBOUR_ARTIST_AFFINITY * weight));
            }
        }
        Ok(result)
    }

    /// Songs by artists the listener does not know yet, found through the ones they play (see
    /// [`FRONTIER_SEEDS`]): the walk scores each new neighbour of the strongest artists, steps
    /// once more from the best of them, then asks the best new artists for their top songs, each
    /// list weighted by its artist's score. The build's budget running out keeps what the walk
    /// has found so far.
    async fn tracks_from_frontier(
        &self,
        artist_scores: &[(String, f64)],
        known: &IgnoreCaseKeys,
        disliked: &IgnoreCaseKeys,
        budget: &Budget<'_>,
    ) -> Vec<Candidate> {
        let seeds: Vec<&(String, f64)> = artist_scores.iter().take(FRONTIER_SEEDS).collect();
        let Some(strongest) = seeds.first().map(|(_, score)| score.max(1e-9)) else {
            return Vec::new();
        };
        let mut scores = FrontierScores::default();
        let first: Vec<(String, f64)> = seeds
            .iter()
            .map(|(artist, score)| (artist.clone(), score / strongest))
            .collect();
        for (similar, weight) in self.neighbours(&first, budget).await {
            scores.add(&similar, weight, known, disliked);
        }
        let step = scores.top(SECOND_STEP_SEEDS);
        if let Some(best) = step.first().map(|(_, score)| score.max(1e-9)) {
            let from: Vec<(String, f64)> = step
                .iter()
                .map(|(artist, score)| (artist.clone(), SECOND_STEP_WEIGHT * score / best))
                .collect();
            for (similar, weight) in self.neighbours(&from, budget).await {
                scores.add(&similar, weight, known, disliked);
            }
        }
        let picked = scores.top(FRONTIER_ARTISTS);
        let Some(top) = picked.first().map(|(_, score)| score.max(1e-9)) else {
            return Vec::new();
        };
        let gate = Semaphore::new(SEEDS_AT_ONCE);
        let lists = futures::future::join_all(picked.iter().map(|(artist, score)| {
            let gate = &gate;
            async move {
                let Ok(_permit) = budget.run(gate.acquire()).await else {
                    return Vec::new();
                };
                let Ok(tracks) = budget
                    .run(self.last_fm.get_artist_top_tracks(artist, FRONTIER_TRACKS_EACH))
                    .await
                else {
                    return Vec::new();
                };
                ranked(tracks, &format!("frontier:{artist}"), score / top)
            }
        }))
        .await;
        lists.into_iter().flatten().collect()
    }

    /// Each artist's similar artists with the weight its answer carries; an artist whose
    /// answer did not come in time, or could not be read, adds nothing.
    async fn neighbours(
        &self,
        from: &[(String, f64)],
        budget: &Budget<'_>,
    ) -> Vec<(Vec<octo_core::last_fm::SimilarArtist>, f64)> {
        futures::future::join_all(from.iter().map(|(artist, weight)| async move {
            match budget
                .run(self.last_fm.get_similar_artists(artist, FRONTIER_NEIGHBOURS))
                .await
            {
                Ok(Ok(similar)) => Some((similar, *weight)),
                _ => None,
            }
        }))
        .await
        .into_iter()
        .flatten()
        .collect()
    }
}

/// The station's previous snapshot, matched by key ignoring case, or `empty`.
fn previous_snapshot<'a>(
    snapshots: &'a [(String, IgnoreCaseKeys)],
    empty: &'a IgnoreCaseKeys,
    station_key: &str,
) -> &'a IgnoreCaseKeys {
    snapshots
        .iter()
        .find(|(key, _)| dotnet::eq_ignore_case(key, station_key))
        .map_or(empty, |(_, keys)| keys)
}

/// Whether `value` appears in `text`, ignoring case.
fn contains_ignore_case(text: &str, value: &str) -> bool {
    dotnet::ordinal_ignore_case_key(text).contains(&dotnet::ordinal_ignore_case_key(value))
}

#[cfg(test)]
#[path = "last_fm_radio_recommendation_service_tests.rs"]
mod tests;
