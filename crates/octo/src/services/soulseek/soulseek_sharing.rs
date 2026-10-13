//! The Sharing card, read from slskd, and Soulseek's port test. The card's records and the
//! readers of slskd's answers are `octo_core::soulseek::soulseek_sharing`.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use octo_core::common::Clock;
use octo_core::settings::SettingsStore;
use octo_core::soulseek::soulseek_sharing::{self, PORT_TEST_URL, parse_listen_port, parse_port_answer};
use octo_core::soulseek::{PortCheckResult, PortState, ShareSwitchState, SharingReport};
use parking_lot::Mutex;
use tracing::info;

use super::soulseek_share_switch::{ISlskdShares, SoulseekShareSwitch};

/// Reads how slskd shares with the Soulseek network and says plainly what is wrong with it.
/// Soulseek works because people share: many users refuse to send files to someone who shares
/// nothing, and a closed listening port stops most people reaching you at all. Everything here
/// reads, apart from asking slskd to look through its shared folders again; turning sharing on
/// and off is [`SoulseekShareSwitch`].
pub struct SoulseekSharing {
    slskd: Arc<dyn ISlskdShares>,
    port: Arc<SoulseekPortCheck>,
    switch: Arc<SoulseekShareSwitch>,
    settings: Arc<SettingsStore>,
    clock: Clock,
    last_rescan_utc: Mutex<Option<DateTime<Utc>>>,
}

impl SoulseekSharing {
    pub fn new(
        slskd: Arc<dyn ISlskdShares>,
        port: Arc<SoulseekPortCheck>,
        switch: Arc<SoulseekShareSwitch>,
        settings: Arc<SettingsStore>,
    ) -> Self {
        SoulseekSharing {
            slskd,
            port,
            switch,
            settings,
            clock: Clock::system(),
            last_rescan_utc: Mutex::new(None),
        }
    }

    /// The report as it stands. `test_port` asks Soulseek's port test now rather than using
    /// its last answer. `share_on` stands in for the saved switch just after it was flipped,
    /// before the settings file has been read back.
    pub async fn report(&self, test_port: bool, share_on: Option<bool>) -> SharingReport {
        let (app, options, shares, uploads) = tokio::join!(
            self.slskd.read("application"),
            self.slskd.read("options"),
            self.slskd.read("shares"),
            self.slskd.read("transfers/uploads?includeRemoved=true"),
        );

        let port = match parse_listen_port(options.as_deref()) {
            Some(port) => self.port.check(port, test_port).await,
            None => PortCheckResult::new(
                if self.settings.current().soulseek.check_listen_port {
                    PortState::Unknown
                } else {
                    PortState::Off
                },
                None,
                None,
                None,
            ),
        };

        let settings = self.settings.current();
        let soulseek = &settings.soulseek;
        let default_login = soulseek.username.as_deref().map(str::trim) == Some("slskd")
            && soulseek.password.as_deref() == Some("slskd");
        let share_switch = ShareSwitchState {
            on: share_on.unwrap_or(soulseek.share_library),
            control: self.switch.control(),
            problem: self.switch.problem(),
        };
        soulseek_sharing::build(
            app.as_deref(),
            options.as_deref(),
            shares.as_deref(),
            uploads.as_deref(),
            port,
            share_switch,
            default_login,
            self.clock.now(),
        )
    }

    /// Starts a share rescan in slskd. None when it started, otherwise why not.
    pub async fn rescan(&self) -> Option<String> {
        let refused = self.slskd.rescan_shares().await;
        if refused.is_none() {
            *self.last_rescan_utc.lock() = Some(Utc::now());
        }
        refused
    }

    /// When a rescan last started from the dashboard, so the activity view watches it.
    pub fn last_rescan_utc(&self) -> Option<DateTime<Utc>> {
        *self.last_rescan_utc.lock()
    }
}

/// Asks Soulseek's own port test whether other people can connect to slskd. The test connects
/// back to the address the request came from, so it answers for this server's internet address;
/// that is slskd's too unless slskd goes out another way, such as through a VPN. It sends the
/// port number and nothing else. One answer is kept for 6 hours, and Test asks again at most
/// every 30 seconds.
pub struct SoulseekPortCheck {
    http: reqwest::Client,
    settings: Arc<SettingsStore>,
    /// The test's address, `{0}` for the port. Only tests point it elsewhere.
    test_url: String,
    clock: Clock,
    last: tokio::sync::Mutex<(Option<PortCheckResult>, Option<DateTime<Utc>>)>,
}

impl SoulseekPortCheck {
    /// How long one answer is kept.
    pub const KEEP_FOR: TimeDelta = TimeDelta::hours(6);
    /// How soon Test may ask again.
    pub const TEST_AT_MOST_EVERY: TimeDelta = TimeDelta::seconds(30);
    /// The port test client's timeout.
    pub const TIMEOUT: Duration = Duration::from_secs(20);

    pub fn new(settings: Arc<SettingsStore>) -> Self {
        Self::with_test_url(settings, PORT_TEST_URL, Clock::system())
    }

    /// A check that asks `test_url` (`{0}` for the port) and reads `clock` for now.
    pub fn with_test_url(settings: Arc<SettingsStore>, test_url: &str, clock: Clock) -> Self {
        SoulseekPortCheck {
            http: reqwest::Client::builder()
                .timeout(Self::TIMEOUT)
                .build()
                .expect("a plain HTTP client builds"),
            settings,
            test_url: test_url.to_string(),
            clock,
            last: tokio::sync::Mutex::new((None, None)),
        }
    }

    pub async fn check(&self, port: i32, force: bool) -> PortCheckResult {
        if !self.settings.current().soulseek.check_listen_port {
            return PortCheckResult::new(PortState::Off, Some(port), None, None);
        }

        let mut last = self.last.lock().await;
        let now = self.clock.now();
        if let (Some(answer), Some(asked_at)) = (&last.0, last.1)
            && answer.port == Some(port)
        {
            let age = now - asked_at;
            let fresh = if force {
                age < Self::TEST_AT_MOST_EVERY
            } else {
                age < Self::KEEP_FOR
            };
            if fresh {
                return answer.clone();
            }
        }

        last.1 = Some(now);
        let url = self.test_url.replace("{0}", &port.to_string());
        let answer = match self.ask(&url).await {
            Ok(page) => {
                let state = parse_port_answer(&page, port);
                PortCheckResult::new(
                    state,
                    Some(port),
                    Some(self.clock.now()),
                    (state == PortState::Unknown)
                        .then(|| "Soulseek's port test gave an answer Octo does not understand.".to_string()),
                )
            }
            Err(e) => {
                info!("Soulseek port test did not answer: {e}");
                PortCheckResult::new(
                    PortState::Unknown,
                    Some(port),
                    Some(self.clock.now()),
                    Some("Soulseek's port test did not answer.".to_string()),
                )
            }
        };
        last.0 = Some(answer.clone());
        answer
    }

    /// The page, or the failure (a status that is not a success included).
    async fn ask(&self, url: &str) -> anyhow::Result<String> {
        let response = self.http.get(url).send().await?;
        let status = response.status();
        if !status.is_success() {
            anyhow::bail!(crate::services::http_client_factory::status_failure_message(
                status
            ));
        }
        Ok(response.text().await?)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::Router;
    use axum::extract::{Query, State};
    use axum::routing::get;
    use chrono::TimeZone;
    use octo_core::settings::{AppSettings, SoulseekSettings};

    use super::*;

    struct Counting {
        answer: Mutex<String>,
        calls: AtomicUsize,
        last_port: Mutex<Option<String>>,
    }

    /// A port test on a local port: counts the calls and answers what it is told.
    async fn port_test(answer: &str) -> (Arc<Counting>, String) {
        let counting = Arc::new(Counting {
            answer: Mutex::new(answer.to_string()),
            calls: AtomicUsize::new(0),
            last_port: Mutex::new(None),
        });
        let app = Router::new()
            .route(
                "/porttest.php",
                get(
                    |State(c): State<Arc<Counting>>,
                     Query(q): Query<std::collections::HashMap<String, String>>| async move {
                        c.calls.fetch_add(1, Ordering::SeqCst);
                        *c.last_port.lock() = q.get("port").cloned();
                        c.answer.lock().clone()
                    },
                ),
            )
            .with_state(counting.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a port");
        let address = listener.local_addr().expect("an address");
        tokio::spawn(async move { axum::serve(listener, app).await });
        (counting, format!("http://{address}/porttest.php?port={{0}}"))
    }

    fn settings(check: bool) -> Arc<SettingsStore> {
        Arc::new(SettingsStore::for_tests(AppSettings {
            soulseek: SoulseekSettings {
                check_listen_port: check,
                ..Default::default()
            },
            ..Default::default()
        }))
    }

    fn start() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 4, 21, 0, 0).unwrap()
    }

    #[tokio::test]
    async fn port_test_is_kept_six_hours_and_test_asks_again_at_most_every_30_seconds() {
        let (counting, url) = port_test("Port: 50300/tcp CLOSED.").await;
        let now = Arc::new(Mutex::new(start()));
        let clock_now = now.clone();
        let check =
            SoulseekPortCheck::with_test_url(settings(true), &url, Clock::new(move || *clock_now.lock()));

        assert_eq!(check.check(50300, false).await.state, PortState::Closed);
        assert_eq!(counting.last_port.lock().as_deref(), Some("50300"));
        check.check(50300, false).await;
        check.check(50300, true).await;
        assert_eq!(counting.calls.load(Ordering::SeqCst), 1);

        *now.lock() = start() + TimeDelta::seconds(31);
        *counting.answer.lock() = "Port: 50300/tcp OPEN.".to_string();
        assert_eq!(check.check(50300, true).await.state, PortState::Open);
        assert_eq!(counting.calls.load(Ordering::SeqCst), 2);

        *now.lock() = start() + TimeDelta::hours(7);
        check.check(50300, false).await;
        assert_eq!(counting.calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn port_test_turned_off_never_asks() {
        let (counting, url) = port_test("Port: 50300/tcp OPEN.").await;
        let check = SoulseekPortCheck::with_test_url(settings(false), &url, Clock::system());

        assert_eq!(check.check(50300, true).await.state, PortState::Off);
        assert_eq!(counting.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn port_test_that_fails_is_unknown_not_closed() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
        let address = listener.local_addr().expect("an address");
        drop(listener);
        let check = SoulseekPortCheck::with_test_url(
            settings(true),
            &format!("http://{address}/porttest.php?port={{0}}"),
            Clock::system(),
        );

        let result = check.check(50300, false).await;

        assert_eq!(result.state, PortState::Unknown);
        assert!(result.error.is_some());
    }
}
