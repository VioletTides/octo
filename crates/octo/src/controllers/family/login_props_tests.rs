//! One login per person, through the whole pipeline: where a member's password may and may not
//! go, every place it signs in, what changing or resetting it ends, which devices get recorded,
//! the sign-in hand-over between a person's own devices, the routes that are gone, Family off,
//! and the page a hand-over QR code opens.

use super::*;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use octo_core::family::codes::subsonic_token;

use crate::logging::redact;
use crate::services::family::member_bridge::{ACCOUNT_FAILURES, MAX_FAILURES};
use crate::services::family::web_session::{WebClaims, mint};
use crate::services::test_support::LogCapture;

const OWNER_PASSWORD: &str = "owner pass";
const BOB_PASSWORD: &str = "bob pass";
/// The hidden Navidrome password every test member has.
const HIDDEN: &str = "hidden";

// ------------------------------------------------------------------------------------------
// Helpers
// ------------------------------------------------------------------------------------------

/// A token shaped like one Navidrome hands out for `sub`.
fn navidrome_jwt(sub: &str, marker: &str) -> String {
    let payload = json!({ "iss": "ND", "sub": sub, "uid": format!("u-{sub}"), "exp": Utc::now().timestamp() + 3600, "n": marker });
    format!(
        "{}.{}.{}",
        URL_SAFE_NO_PAD.encode(r#"{"alg":"HS256","typ":"JWT"}"#),
        URL_SAFE_NO_PAD.encode(payload.to_string()),
        URL_SAFE_NO_PAD.encode(format!("navidrome-signature-{marker}"))
    )
}

/// A token shaped like Octo's own for `sub` and `device`, signed with a key that is not this
/// server's.
fn forged_octo_token(sub: &str, device: &str) -> String {
    let now = Utc::now().timestamp();
    mint(
        &[7u8; 32],
        &WebClaims {
            iss: "ND".into(),
            sub: sub.into(),
            uid: format!("u-{sub}"),
            adm: false,
            iat: now,
            exp: now + 3600,
            did: device.into(),
        },
    )
}

/// Navidrome takes only the owner's and Bob's own passwords and the members' hidden one; every
/// other sign-in it is asked about is wrong.
struct OnlyRealSignIns;

impl wiremock::Match for OnlyRealSignIns {
    fn matches(&self, request: &wiremock::Request) -> bool {
        let pairs: Vec<(String, String)> = request
            .url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        let get = |key: &str| pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());
        let right = match (get("u"), get("p"), get("t"), get("s")) {
            (Some("winters"), Some(OWNER_PASSWORD), None, None) => true,
            (Some("bob"), Some(BOB_PASSWORD), None, None) => true,
            (Some("alex" | "sam"), None, Some(t), Some(s)) => subsonic_token(HIDDEN, s) == t,
            _ => false,
        };
        // A match here is the refusal.
        !right
    }
}

/// Mounts [`OnlyRealSignIns`] in front of the harness's ping, which takes anything.
async fn only_real_sign_ins(f: &Fam) {
    Mock::given(method("GET"))
        .and(path("/rest/ping"))
        .and(OnlyRealSignIns)
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"{"subsonic-response":{"status":"failed","version":"1.16.1","error":{"code":40,"message":"Wrong username or password"}}}"#,
            "application/json",
        ))
        .with_priority(1)
        .mount(&f.nd)
        .await;
}

/// Navidrome's web player sign-in for a member signed in with the hidden password, answering
/// `real` after `delay`.
async fn member_web_login(f: &Fam, user: &str, real: &str, delay: Duration) {
    Mock::given(method("POST"))
        .and(path("/auth/login"))
        .and(body_partial_json(json!({ "username": user, "password": HIDDEN })))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(
                    json!({ "id": format!("u-{user}"), "isAdmin": false, "token": real, "username": user }),
                )
                .set_delay(delay),
        )
        .mount(&f.nd)
        .await;
}

/// Signs `user` in on Navidrome's web player with `password`, from `from`.
async fn web_login(app: &App, user: &str, password: &str, from: &str) -> Reply {
    raw(
        app,
        Method::POST,
        "/auth/login",
        &[("content-type", "application/json".to_string())],
        json!({ "username": user, "password": password }).to_string(),
        from,
    )
    .await
}

/// A call to Navidrome's own API carrying the browser token `token`.
async fn native_call(app: &App, token: &str) -> Reply {
    raw(
        app,
        Method::GET,
        "/api/queue",
        &[("x-nd-authorization", format!("Bearer {token}"))],
        String::new(),
        HOME,
    )
    .await
}

/// Navidrome's queue answers any web player call, after `delay`, handing back `fresh` as a new
/// token when given.
async fn queue_answers(f: &Fam, fresh: Option<&str>, delay: Duration) {
    let mut answer = ResponseTemplate::new(200)
        .set_body_json(json!({ "items": [] }))
        .set_delay(delay);
    if let Some(fresh) = fresh {
        answer = answer.insert_header("x-nd-authorization", fresh);
    }
    Mock::given(method("GET"))
        .and(path("/api/queue"))
        .respond_with(answer)
        .mount(&f.nd)
        .await;
}

/// Waits until Navidrome has been asked for `wanted`.
async fn until_asked(f: &Fam, wanted: &str) {
    for _ in 0..200 {
        let asked = f.nd.received_requests().await.unwrap_or_default();
        if asked.iter().any(|r| r.url.path() == wanted) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("Navidrome was never asked for {wanted}");
}

/// A Subsonic call as `user` signed with the token form of `password`.
async fn rest_token(f: &Fam, user: &str, password: &str, endpoint: &str, from: &str) -> Reply {
    let salt = "c0ffee";
    let t = subsonic_token(password, salt);
    let uri = format!("/rest/{endpoint}?u={user}&t={t}&s={salt}&v=1.16.1&c=test&f=json");
    raw(&f.app, Method::GET, &uri, &[], String::new(), from).await
}

/// A Subsonic call with whatever sign-in `query` carries.
async fn rest_query(f: &Fam, endpoint: &str, query: &str, from: &str) -> Reply {
    let uri = format!("/rest/{endpoint}?{query}&v=1.16.1&c=test&f=json");
    raw(&f.app, Method::GET, &uri, &[], String::new(), from).await
}

/// A Subsonic call as `user` with `password` in a form body.
async fn rest_form(f: &Fam, endpoint: &str, form: &str, from: &str) -> Reply {
    raw(
        &f.app,
        Method::POST,
        &format!("/rest/{endpoint}"),
        &[("content-type", "application/x-www-form-urlencoded".to_string())],
        format!("{form}&v=1.16.1&c=test&f=json"),
        from,
    )
    .await
}

fn ok(reply: &Reply) -> bool {
    serde_json::from_str::<Value>(&reply.body)
        .ok()
        .is_some_and(|v| v["subsonic-response"]["status"] == "ok")
}

fn code(reply: &Reply) -> Value {
    serde_json::from_str::<Value>(&reply.body)
        .map(|v| v["subsonic-response"]["error"]["code"].clone())
        .unwrap_or(Value::Null)
}

/// Every header value and the body of a reply, as text.
fn everything(reply: &Reply) -> String {
    let mut text = reply.body.clone();
    for (name, value) in &reply.headers {
        text.push_str(&format!(
            "\n{name}: {}",
            String::from_utf8_lossy(value.as_bytes())
        ));
    }
    text
}

/// Every line logged while `capture` ran, as the log writer writes it (redacted).
fn written(capture: &LogCapture) -> Vec<String> {
    use tracing::Level;
    [Level::ERROR, Level::WARN, Level::INFO, Level::DEBUG, Level::TRACE]
        .into_iter()
        .flat_map(|level| capture.at(level))
        .map(|line| redact(&line).into_owned())
        .collect()
}

///  escaped for a query.
fn esc(text: &str) -> String {
    octo_subsonic::subsonic_request_parser::escape_data_string(text)
}

fn token_of(start: &Value) -> String {
    start["token"].as_str().expect("a token").to_string()
}

/// A new invite for `user` from a manager, as its token.
async fn new_invite(f: &Fam, user: &str) -> String {
    let r = f
        .post(
            &format!("/api/family/members/{user}/invite"),
            Some(&f.sam()),
            json!({}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let link = r.json()["inviteLink"].as_str().expect("a link").to_string();
    link.split("#invite=").nth(1).expect("a token").to_string()
}

async fn join_with(f: &Fam, token: &str, password: &str) -> Reply {
    f.post(
        "/api/family/join",
        None,
        json!({ "token": token, "password": password }),
    )
    .await
}

/// A radio station session started by `user`, as the radio stream's token.
fn radio_session(f: &Fam, user: &str) -> String {
    let salt = "abcdef";
    let t = subsonic_token(HIDDEN, salt);
    f.state.last_fm_radio_stream_sessions.issue(
        user,
        "station-1",
        [("u", user), ("t", t.as_str()), ("s", salt)],
        None,
    )
}

/// Everything a member could be signed in with before a password change or reset, so the test
/// can check each ends.
struct Sessions {
    page: String,
    web: String,
    radio: String,
    hand_over: String,
    hand_over_id: String,
    invite: String,
}

async fn sign_alex_in_everywhere(f: &Fam) -> Sessions {
    let real = navidrome_jwt("alex", "real");
    member_web_login(f, "alex", &real, Duration::ZERO).await;
    queue_answers(f, None, Duration::ZERO).await;
    let page = f.alex();
    let login = web_login(&f.app, "alex", ALEX_PASSWORD, HOME).await;
    assert_eq!(login.status, StatusCode::OK, "{}", login.body);
    let web = login.json()["token"].as_str().expect("a token").to_string();
    assert_eq!(native_call(&f.app, &web).await.status, StatusCode::OK);
    let radio = radio_session(f, "alex");
    assert!(f.state.last_fm_radio_stream_sessions.get(&radio, None).is_some());
    let hand_over = token_of(&start_hand_over(f, &page).await);
    let redeemed = redeem_from(f, &hand_over, AWAY).await;
    assert_eq!(redeemed.status, StatusCode::OK, "{}", redeemed.body);
    let hand_over_id = redeemed.json()["id"].as_str().expect("an id").to_string();
    let invite = new_invite(f, "alex").await;
    Sessions {
        page,
        web,
        radio,
        hand_over,
        hand_over_id,
        invite,
    }
}

/// Every session in `before` has ended, and `old` signs nothing in anywhere.
async fn all_ended(f: &Fam, before: &Sessions, old: &str, fresh_page: &str) {
    assert_eq!(code(&rest_with(f, "alex", old, "test", HOME).await), 40);
    assert_eq!(code(&rest_token(f, "alex", old, "getFamily", HOME).await), 40);
    let page = f
        .post(
            "/api/family/auth",
            None,
            json!({ "username": "alex", "password": old }),
        )
        .await;
    assert_eq!(page.status, StatusCode::UNAUTHORIZED, "{}", page.body);
    assert_eq!(
        web_login(&f.app, "alex", old, HOME).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        f.get("/api/family/session", Some(&before.page)).await.status,
        StatusCode::UNAUTHORIZED,
        "the other Family page session"
    );
    assert_eq!(
        native_call(&f.app, &before.web).await.status,
        StatusCode::UNAUTHORIZED,
        "the web player session"
    );
    assert_eq!(
        code(&rest_query(f, "getFamily", &format!("jwt={}", before.web), HOME).await),
        40,
        "the web player's token on /rest"
    );
    assert!(
        f.state
            .family_store
            .devices_of("alex")
            .iter()
            .all(|d| d.navidrome_jwt.is_none())
    );
    assert!(
        f.state
            .last_fm_radio_stream_sessions
            .get(&before.radio, None)
            .is_none(),
        "the radio stream started before"
    );
    let pending = f
        .get(
            &format!("/api/family/signin/{}/pending", before.hand_over),
            Some(fresh_page),
        )
        .await;
    assert_eq!(
        pending.status,
        StatusCode::NOT_FOUND,
        "the hand-over under way: {}",
        pending.body
    );
    assert_eq!(
        fetch_box(f, &before.hand_over_id).await,
        json!({ "state": "Expired" })
    );
    let joined = join_with(f, &before.invite, "taken-over-password").await;
    assert_eq!(
        joined.status,
        StatusCode::FORBIDDEN,
        "a sign-up link sent before: {}",
        joined.body
    );
}

// ------------------------------------------------------------------------------------------
// The member password is kept sealed and goes nowhere
// ------------------------------------------------------------------------------------------

/// The account password, in clear, hex or sealed, is never in an answer, a device or session
/// record, or a line of the log, through signing up, signing in everywhere and changing it.
#[tokio::test]
async fn a_member_password_is_never_in_an_answer_a_device_or_session_record_or_a_log_line() {
    let f = fam().await;
    let (capture, _guard) = LogCapture::start();
    let real = navidrome_jwt("alex", "real");
    member_web_login(&f, "alex", &real, Duration::ZERO).await;
    queue_answers(&f, None, Duration::ZERO).await;
    let joined = "joined-password-1";
    let changed = "changed-password-2";
    let mut replies: Vec<Reply> = Vec::new();

    let reset = f
        .post("/api/family/members/alex/reset", Some(&f.sam()), json!({}))
        .await;
    let link = reset.json()["inviteLink"].as_str().expect("a link").to_string();
    replies.push(reset);
    let invite = link.split("#invite=").nth(1).expect("a token").to_string();
    replies.push(join_with(&f, &invite, joined).await);
    // The apps' change, sent both ways the call takes.
    replies.push(
        rest_form(
            &f,
            "changeFamilyPassword",
            &format!("u=alex&p={joined}&current={joined}&next=interim-password-3"),
            HOME,
        )
        .await,
    );
    replies.push(
        rest_query(
            &f,
            "changeFamilyPassword",
            &format!("u=alex&p=interim-password-3&current=interim-password-3&next={changed}"),
            HOME,
        )
        .await,
    );
    assert!(
        ok(replies.last().expect("a reply")),
        "{}",
        replies.last().expect("a reply").body
    );
    let cookie = format!(
        "octo_family={}",
        f.post(
            "/api/family/auth",
            None,
            json!({ "username": "alex", "password": changed }),
        )
        .await
        .cookie_token()
    );
    for uri in [
        "/api/family/session",
        "/api/family/me",
        "/api/family/me/login",
        "/api/family/me/devices",
    ] {
        replies.push(f.get(uri, Some(&cookie)).await);
    }
    for uri in ["/api/family/members", "/api/family/devices"] {
        replies.push(f.get(uri, Some(&f.sam())).await);
    }
    replies.push(f.post("/api/family/signin/start", Some(&cookie), json!({})).await);
    for endpoint in [
        "getFamily",
        "getFamilyLogin",
        "getFamilyDevices",
        "startFamilySignIn",
    ] {
        replies.push(rest_token(&f, "alex", changed, endpoint, HOME).await);
    }
    replies.push(rest_query(&f, "getFamilyDevices", "u=sam&p=sam-password&all=true", HOME).await);
    let web = web_login(&f.app, "alex", changed, HOME).await;
    assert_eq!(web.status, StatusCode::OK, "{}", web.body);
    replies.push(web);
    replies.push(
        raw(
            &f.app,
            Method::GET,
            &format!(
                "/api/family/me?u=alex&p=enc:{}&v=1.16.1&c=test",
                hex::encode(changed)
            ),
            &[],
            String::new(),
            HOME,
        )
        .await,
    );

    let member = f.state.family_store.member("alex").expect("alex");
    let sealed = member.account_password.clone();
    assert!(passwords::matches(&f.state.family_vault, &sealed, changed));
    let secrets: Vec<String> = [joined, "interim-password-3", changed]
        .iter()
        .flat_map(|password| [password.to_string(), hex::encode(password)])
        .chain([sealed.clone()])
        .collect();
    for reply in &replies {
        let text = everything(reply);
        for secret in &secrets {
            assert!(!text.contains(secret.as_str()), "{secret} in {text}");
        }
    }
    let devices = serde_json::to_string(&f.state.family_store.devices()).expect("written");
    let detected = serde_json::to_string(&f.state.family_store.detected()).expect("written");
    let lines = written(&capture);
    assert!(!lines.is_empty(), "requests were logged");
    for secret in &secrets {
        assert!(!devices.contains(secret.as_str()), "{secret} in a device record");
        assert!(!detected.contains(secret.as_str()), "{secret} in a device record");
        for line in &lines {
            assert!(!line.contains(secret.as_str()), "{secret} logged: {line}");
        }
    }
    // Sessions are kept on disk by a hash of their token and the username alone.
    f.state.family_store.flush_if_dirty();
    for entry in std::fs::read_dir(f._dir.path()).expect("the config folder") {
        let path = entry.expect("an entry").path();
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for password in [joined, "interim-password-3", changed] {
            assert!(!text.contains(password), "{password} in {}", path.display());
        }
        if path.file_name().is_some_and(|n| n != "family.json") {
            assert!(
                !text.contains(&sealed),
                "the sealed password in {}",
                path.display()
            );
        }
    }
}

/// `family.json` keeps only sealed passwords, which open with this server's `family.key` and
/// nothing else: without it no member's password can be had, and none signs in.
#[tokio::test]
async fn a_store_file_without_its_key_yields_no_member_password() {
    let f = fam().await;
    f.state.family_store.set_owner("winters");
    let file = std::fs::read_to_string(f._dir.path().join("family.json")).expect("written");
    for password in [ALEX_PASSWORD, SAM_PASSWORD] {
        assert!(!file.contains(password), "{password} in {file}");
        assert!(!file.contains(&hex::encode(password)), "{file}");
    }
    let stored: Vec<String> = f
        .state
        .family_store
        .members()
        .into_iter()
        .map(|m| m.account_password)
        .collect();
    assert_eq!(stored.len(), 2);
    let kept: Value = serde_json::from_str(&file).expect("json");
    let kept: Vec<&str> = kept["Members"]
        .as_array()
        .expect("members")
        .iter()
        .filter_map(|m| m["AccountPassword"].as_str())
        .collect();
    for sealed in &stored {
        assert!(kept.contains(&sealed.as_str()), "{kept:?}");
        assert!(passwords::open(&f.state.family_vault, sealed).is_some());
        assert_eq!(
            passwords::open(
                &crate::services::family::vault::FamilyVault::without_key(),
                sealed
            ),
            None
        );
        assert_eq!(
            passwords::open(&crate::services::family::vault::FamilyVault::in_memory(), sealed),
            None,
            "another key opens nothing"
        );
    }
    // The same files without the key: nobody's password opens, so nobody signs in with it.
    std::fs::remove_file(f._dir.path().join("family.key")).expect("removed");
    let keyless = AppState::for_tests_with(
        AppSettings {
            family: FamilySettings {
                enabled: true,
                ..Default::default()
            },
            subsonic: SubsonicSettings {
                url: Some(f.nd.uri()),
                ..Default::default()
            },
            ..Default::default()
        },
        TestServices {
            config_dir: Some(f._dir.path().to_path_buf()),
            ..Default::default()
        },
    );
    assert!(!keyless.family_vault.has_key());
    for member in keyless.family_store.members() {
        assert_eq!(
            passwords::open(&keyless.family_vault, &member.account_password),
            None
        );
    }
    let app = build_with(
        keyless.clone(),
        app_routes(&StaticAssets::load(&StaticRoots::from_env())),
    );
    let uri = format!("/rest/getFamily?u=alex&p={ALEX_PASSWORD}&v=1.16.1&c=test&f=json");
    let r = raw(&app, Method::GET, &uri, &[], String::new(), HOME).await;
    assert_eq!(code(&r), 40, "{}", r.body);
}

// ------------------------------------------------------------------------------------------
// Signing in
// ------------------------------------------------------------------------------------------

/// The one password signs in `/rest` (plain, `enc:`, token, in the query or a form body), the
/// signed `/api/family` calls, the Family page and Navidrome's web player.
#[tokio::test]
async fn the_account_password_signs_in_on_every_surface_and_in_every_form() {
    let f = fam().await;
    let enc = format!("enc:{}", hex::encode(ALEX_PASSWORD));
    let enc_upper = format!("enc:{}", hex::encode_upper(ALEX_PASSWORD));
    let salt = "5a1t";
    let t = subsonic_token(ALEX_PASSWORD, salt);
    let forms = [
        format!("u=alex&p={ALEX_PASSWORD}"),
        format!("u=alex&p={enc}"),
        format!("u=alex&p={enc_upper}"),
        format!("u=alex&t={t}&s={salt}"),
        format!("u=alex&t={}&s={salt}", t.to_uppercase()),
    ];
    for form in &forms {
        let r = rest_query(&f, "getFamily", form, HOME).await;
        assert!(ok(&r), "{form}: {}", r.body);
        let r = rest_form(&f, "getFamily", form, HOME).await;
        assert!(ok(&r), "{form} in a form body: {}", r.body);
        let signed = raw(
            &f.app,
            Method::GET,
            &format!("/api/family/me?{form}&v=1.16.1&c=test"),
            &[],
            String::new(),
            HOME,
        )
        .await;
        assert_eq!(signed.status, StatusCode::OK, "{form}: {}", signed.body);
    }
    let page = f
        .post(
            "/api/family/auth",
            None,
            json!({ "username": "alex", "password": ALEX_PASSWORD }),
        )
        .await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    member_web_login(&f, "alex", &navidrome_jwt("alex", "real"), Duration::ZERO).await;
    let web = web_login(&f.app, "alex", ALEX_PASSWORD, HOME).await;
    assert_eq!(web.status, StatusCode::OK, "{}", web.body);
    // Every one of them refuses another password.
    for form in [
        "u=alex&p=not-the-password".to_string(),
        format!("u=alex&p=enc:{}", hex::encode("not-the-password")),
        format!("u=alex&t={}&s={salt}", subsonic_token("not-the-password", salt)),
    ] {
        assert_eq!(
            code(&rest_query(&f, "getFamily", &form, HOME).await),
            40,
            "{form}"
        );
    }
}

/// A wrong member password counts toward the same throttles whatever surface it was sent to:
/// guesses on the Family page sign-in hold `/rest` back for that account, and the other way
/// round.
#[tokio::test]
async fn wrong_member_passwords_count_toward_every_throttle_from_every_surface() {
    // Guesses on the Family page.
    let f = fam().await;
    let mut guesses = 0;
    for a in 0..(ACCOUNT_FAILURES / 10) {
        for g in 0..10 {
            let r = f
                .call(
                    Method::POST,
                    "/api/family/auth",
                    None,
                    Some(json!({ "username": "alex", "password": format!("guess-{a}-{g}") })),
                    &format!("10.40.{a}.1"),
                )
                .await;
            assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.body);
            guesses += 1;
        }
    }
    assert_eq!(guesses, ACCOUNT_FAILURES);
    let r = rest_with(&f, "alex", ALEX_PASSWORD, "test", "10.41.0.1").await;
    assert_eq!(
        code(&r),
        40,
        "a hundred page guesses hold the account back on /rest too: {}",
        r.body
    );
    // Guesses on /rest.
    let f = fam().await;
    for a in 0..(ACCOUNT_FAILURES / MAX_FAILURES) {
        for _ in 0..MAX_FAILURES {
            let r = rest_with(&f, "alex", "not-the-password", "test", &format!("10.50.{a}.1")).await;
            assert_eq!(code(&r), 40);
        }
    }
    let r = f
        .call(
            Method::POST,
            "/api/family/auth",
            None,
            Some(json!({ "username": "alex", "password": ALEX_PASSWORD })),
            "10.51.0.1",
        )
        .await;
    assert_ne!(
        r.status,
        StatusCode::OK,
        "a hundred /rest guesses hold the account back on the Family page too"
    );
}

/// A wrong current password on a password change counts as a wrong sign-in, so a session that
/// does not know the password cannot guess it there.
#[tokio::test]
async fn a_wrong_current_password_counts_toward_the_sign_in_throttles() {
    let f = fam().await;
    let from = "10.7.7.7";
    let cookie = f.alex();
    for n in 0..MAX_FAILURES {
        let r = f
            .call(
                Method::PUT,
                "/api/family/me/password",
                Some(&cookie),
                Some(json!({ "current": format!("guess-{n}"), "next": "a new password" })),
                from,
            )
            .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.body);
    }
    let r = f
        .call(
            Method::PUT,
            "/api/family/me/password",
            Some(&cookie),
            Some(json!({ "current": ALEX_PASSWORD, "next": "a new password" })),
            from,
        )
        .await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS, "{}", r.body);
    assert_eq!(
        code(&rest_with(&f, "alex", ALEX_PASSWORD, "test", from).await),
        40,
        "the guesses count for that address on /rest too"
    );
    // The apps' change counts the same way.
    let other = "10.7.7.8";
    for n in 0..MAX_FAILURES {
        let r = rest_form(
            &f,
            "changeFamilyPassword",
            &format!("u=alex&p={ALEX_PASSWORD}&current=guess-{n}&next=a%20new%20password"),
            other,
        )
        .await;
        assert_ne!(code(&r), Value::Null, "{}", r.body);
    }
    assert_eq!(
        code(&rest_with(&f, "alex", ALEX_PASSWORD, "test", other).await),
        40
    );
    assert!(passwords::matches(
        &f.state.family_vault,
        &f.state
            .family_store
            .member("alex")
            .expect("alex")
            .account_password,
        ALEX_PASSWORD
    ));
}

/// No forged token, made-up key or wrong password reaches the new routes as a member or a
/// manager: not the login, the password change, the reset, removing a device, or any step of
/// a hand-over.
#[tokio::test]
async fn unchecked_callers_never_act_as_a_member_or_a_manager_on_the_new_routes() {
    let f = fam().await;
    only_real_sign_ins(&f).await;
    // Alex's own app at home, so the wrong tries below from away never hold it back.
    assert!(ok(&rest_with(&f, "alex", ALEX_PASSWORD, "test", HOME).await));
    let start = start_hand_over(&f, &f.alex()).await;
    let token = token_of(&start);
    let redeemed = redeem_from(&f, &token, AWAY).await;
    let id = redeemed.json()["id"].as_str().expect("an id").to_string();
    let sign_ins: Vec<(String, Vec<(&str, String)>)> = {
        let mut list = Vec::new();
        for forged in [
            navidrome_jwt("alex", "forged"),
            navidrome_jwt("winters", "forged"),
            navidrome_jwt("sam", "forged"),
            forged_octo_token("alex", "d_app"),
            forged_octo_token("sam", "d_sam"),
            forged_octo_token("winters", "x_owner"),
        ] {
            list.push((format!("jwt={forged}"), Vec::new()));
            list.push((format!("u=alex&jwt={forged}"), Vec::new()));
            list.push((format!("u=winters&jwt={forged}"), Vec::new()));
            list.push((
                String::new(),
                vec![("x-nd-authorization", format!("Bearer {forged}"))],
            ));
            list.push((String::new(), vec![("authorization", format!("Bearer {forged}"))]));
        }
        list.push(("apiKey=not-a-key".into(), Vec::new()));
        for user in ["alex", "sam", "winters", "bob"] {
            list.push((format!("u={user}&p=wrong-password"), Vec::new()));
            list.push((
                format!("u={user}&t={}&s=aa", subsonic_token("wrong-password", "aa")),
                Vec::new(),
            ));
        }
        list
    };
    let rest_calls = [
        "getFamilyLogin".to_string(),
        "startFamilySignIn".to_string(),
        format!("getFamilySignInPending&token={token}"),
        format!("decideFamilySignIn&token={token}&allow=true"),
        format!("putFamilySignInBox&token={token}&box=taken"),
        "changeFamilyPassword&current=x&next=taken-over-1".to_string(),
        "forgetFamilyDevice&id=d_app".to_string(),
        "getFamilyDevices&all=true".to_string(),
    ];
    for (sign_in, headers) in &sign_ins {
        for call in &rest_calls {
            let (endpoint, rest) = call.split_once('&').unwrap_or((call.as_str(), ""));
            let uri = format!("/rest/{endpoint}?{sign_in}&{rest}&v=1.16.1&c=evil&f=json");
            let r = raw(&f.app, Method::GET, &uri, headers, String::new(), AWAY).await;
            assert!(!ok(&r), "{uri} {headers:?}: {}", r.body);
        }
        let api_calls = [
            (Method::GET, "/api/family/me/login".to_string(), None),
            (
                Method::POST,
                "/api/family/signin/start".to_string(),
                Some(json!({})),
            ),
            (Method::GET, format!("/api/family/signin/{token}/pending"), None),
            (
                Method::POST,
                format!("/api/family/signin/{token}/decide"),
                Some(json!({ "allow": true })),
            ),
            (
                Method::POST,
                format!("/api/family/signin/{token}/box"),
                Some(json!({ "box": "taken" })),
            ),
            (
                Method::PUT,
                "/api/family/me/password".to_string(),
                Some(json!({ "current": "x", "next": "taken-over-1" })),
            ),
            (
                Method::POST,
                "/api/family/members/alex/reset".to_string(),
                Some(json!({})),
            ),
            (Method::DELETE, "/api/family/me/devices/d_app".to_string(), None),
        ];
        for (verb, path, body) in api_calls {
            let uri = if sign_in.is_empty() {
                path.clone()
            } else {
                format!("{path}?{sign_in}&v=1.16.1&c=evil")
            };
            let mut sent = headers.clone();
            sent.push(("x-octo-family", "1".to_string()));
            let text = body.map(|b| b.to_string()).unwrap_or_default();
            if !text.is_empty() {
                sent.push(("content-type", "application/json".to_string()));
            }
            let r = raw(&f.app, verb.clone(), &uri, &sent, text, AWAY).await;
            assert!(
                r.status == StatusCode::UNAUTHORIZED || r.status == StatusCode::FORBIDDEN,
                "{verb} {uri} {headers:?}: {} {}",
                r.status,
                r.body
            );
        }
    }
    // Nothing moved: the hand-over still waits, alex's password and devices are as they were.
    assert_eq!(fetch_box(&f, &id).await, json!({ "state": "Waiting" }));
    let alex = f.state.family_store.member("alex").expect("alex");
    assert!(passwords::matches(
        &f.state.family_vault,
        &alex.account_password,
        ALEX_PASSWORD
    ));
    assert!(f.state.family_store.device("d_app").is_some());
    // The real sign-ins still work.
    assert!(ok(&rest_with(&f, "alex", ALEX_PASSWORD, "test", HOME).await));
    assert!(ok(&f.rest_as_owner("startFamilySignIn", "").await));
}

// ------------------------------------------------------------------------------------------
// Changing and resetting a password
// ------------------------------------------------------------------------------------------

/// Changing the password ends everything signed in with the old one at once: Subsonic sign-ins
/// with it, the web player, the other Family page sessions, radio streams, hand-overs under way
/// and sign-up links sent earlier. The new one works everywhere.
#[tokio::test]
async fn changing_the_password_ends_every_other_session_at_once() {
    let f = fam().await;
    let before = sign_alex_in_everywhere(&f).await;
    let changer = f.alex();
    let r = f
        .put(
            "/api/family/me/password",
            Some(&changer),
            json!({ "current": ALEX_PASSWORD, "next": "a-new-password" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.body);
    let fresh = format!("octo_family={}", r.cookie_token());
    all_ended(&f, &before, ALEX_PASSWORD, &fresh).await;
    assert!(ok(&rest_with(&f, "alex", "a-new-password", "test", HOME).await));
    assert!(ok(
        &rest_token(&f, "alex", "a-new-password", "getFamily", HOME).await
    ));
    assert_eq!(
        f.get("/api/family/session", Some(&fresh)).await.status,
        StatusCode::OK
    );
}

/// The apps' own change ends the same sessions.
#[tokio::test]
async fn changing_the_password_from_an_app_ends_every_other_session_at_once() {
    let f = fam().await;
    let before = sign_alex_in_everywhere(&f).await;
    let r = rest_form(
        &f,
        "changeFamilyPassword",
        &format!("u=alex&p={ALEX_PASSWORD}&current={ALEX_PASSWORD}&next=a-new-password"),
        HOME,
    )
    .await;
    assert!(ok(&r), "{}", r.body);
    let fresh = f.alex();
    all_ended(&f, &before, ALEX_PASSWORD, &fresh).await;
}

/// A web player call still with Navidrome while the password changes cannot bring its session
/// back with the fresh token Navidrome answers.
#[tokio::test]
async fn a_web_player_call_in_flight_during_a_password_change_never_brings_its_session_back() {
    let f = fam().await;
    member_web_login(&f, "alex", &navidrome_jwt("alex", "real"), Duration::ZERO).await;
    let login = web_login(&f.app, "alex", ALEX_PASSWORD, HOME).await;
    let web = login.json()["token"].as_str().expect("a token").to_string();
    queue_answers(
        &f,
        Some(&format!("Bearer {}", navidrome_jwt("alex", "fresh"))),
        Duration::from_millis(400),
    )
    .await;
    let in_flight = {
        let (app, web) = (f.app.clone(), web.clone());
        tokio::spawn(async move { native_call(&app, &web).await })
    };
    until_asked(&f, "/api/queue").await;
    let r = f
        .put(
            "/api/family/me/password",
            Some(&f.alex()),
            json!({ "current": ALEX_PASSWORD, "next": "a-new-password" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.body);
    let answered = in_flight.await.expect("the call ended");
    assert!(
        f.state
            .family_store
            .devices_of("alex")
            .iter()
            .all(|d| d.navidrome_jwt.is_none()),
        "the web player session came back"
    );
    let after = native_call(&f.app, &web).await;
    assert_eq!(after.status, StatusCode::UNAUTHORIZED, "{}", after.body);
    if let Some(fresh) = answered
        .headers
        .get("x-nd-authorization")
        .and_then(|v| v.to_str().ok())
    {
        let token = fresh.trim_start_matches("Bearer ").to_string();
        assert_eq!(native_call(&f.app, &token).await.status, StatusCode::UNAUTHORIZED);
    }
}

/// A web player sign-in checked against the old password, still with Navidrome while the
/// password changes, makes no session.
#[tokio::test]
async fn a_web_player_sign_in_checked_before_a_password_change_never_outlives_it() {
    let f = fam().await;
    member_web_login(
        &f,
        "alex",
        &navidrome_jwt("alex", "real"),
        Duration::from_millis(400),
    )
    .await;
    queue_answers(&f, None, Duration::ZERO).await;
    let in_flight = {
        let app = f.app.clone();
        tokio::spawn(async move { web_login(&app, "alex", ALEX_PASSWORD, HOME).await })
    };
    until_asked(&f, "/auth/login").await;
    let r = f
        .put(
            "/api/family/me/password",
            Some(&f.alex()),
            json!({ "current": ALEX_PASSWORD, "next": "a-new-password" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.body);
    let login = in_flight.await.expect("the sign-in ended");
    assert!(
        f.state
            .family_store
            .devices_of("alex")
            .iter()
            .all(|d| d.navidrome_jwt.is_none()),
        "a web player session outlived the password it was checked with"
    );
    if login.status == StatusCode::OK {
        let token = login.json()["token"].as_str().unwrap_or_default().to_string();
        assert_eq!(native_call(&f.app, &token).await.status, StatusCode::UNAUTHORIZED);
    }
}

/// A reset ends everything the old password signed in, and only its own new link sets a new
/// password, once.
#[tokio::test]
async fn resetting_a_password_ends_every_session_and_only_its_new_link_works_once() {
    let f = fam().await;
    let before = sign_alex_in_everywhere(&f).await;
    let r = f
        .post("/api/family/members/alex/reset", Some(&f.sam()), json!({}))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let link = r.json()["inviteLink"].as_str().expect("a link").to_string();
    let token = link.split("#invite=").nth(1).expect("a token").to_string();
    // Before the new link is used, a Family page cookie cannot be had at all.
    all_ended(&f, &before, ALEX_PASSWORD, &f.alex()).await;
    let joined = join_with(&f, &token, "chosen-again-1").await;
    assert_eq!(joined.status, StatusCode::OK, "{}", joined.body);
    assert_eq!(
        join_with(&f, &token, "chosen-again-2").await.status,
        StatusCode::FORBIDDEN,
        "the new link works once"
    );
    assert!(ok(&rest_with(&f, "alex", "chosen-again-1", "test", HOME).await));
    assert_eq!(
        code(&rest_with(&f, "alex", "chosen-again-2", "test", HOME).await),
        40
    );
}

// ------------------------------------------------------------------------------------------
// Devices
// ------------------------------------------------------------------------------------------

/// A flood of wrong sign-ins under made-up apps, installs and names, for members and for names
/// Navidrome does not take, records no device, sends no notice, and keeps the throttle's
/// tables small.
#[tokio::test]
async fn a_flood_of_wrong_sign_ins_under_made_up_devices_records_nothing() {
    let f = fam().await;
    only_real_sign_ins(&f).await;
    let mut notices = f.state.family_devices.subscribe();
    let devices = f.state.family_store.devices();
    let detected = f.state.family_store.detected();
    for n in 0..60 {
        let from = format!("10.60.{}.{}", n / 200, n % 200 + 1);
        let headers = vec![
            ("x-octo-device-id", format!("made-up-install-{n}")),
            ("x-octo-device-name", format!("Phone {n}")),
            ("user-agent", format!("App{n}/1.0")),
        ];
        for user in ["alex", "winters", "nobody"] {
            let uri = format!("/rest/getFamily?u={user}&p=wrong-{n}&v=1.16.1&c=app{n}&f=json");
            let r = raw(&f.app, Method::GET, &uri, &headers, String::new(), &from).await;
            assert!(!ok(&r), "{}", r.body);
            let uri = format!("/api/family/me?u={user}&p=wrong-{n}&v=1.16.1&c=app{n}");
            let r = raw(&f.app, Method::GET, &uri, &headers, String::new(), &from).await;
            assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.body);
        }
        let r = raw(
            &f.app,
            Method::POST,
            "/auth/login",
            &[
                ("content-type", "application/json".to_string()),
                ("x-nd-client-unique-id", format!("browser-{n}")),
            ],
            json!({ "username": "alex", "password": format!("wrong-{n}") }).to_string(),
            &from,
        )
        .await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(f.state.family_store.devices(), devices);
    assert_eq!(f.state.family_store.detected().len(), detected.len());
    assert!(notices.try_recv().is_err(), "a notice went out");
    assert!(f.state.family_bridge.throttle.len() <= 10_000);
    assert!(f.state.family_bridge.known.len() <= 10_000);
}

/// However many installs sign in with the right password, one member's device list stays
/// bounded, keeping the ones seen last.
#[tokio::test]
async fn a_members_device_list_stays_bounded_however_many_installs_sign_in() {
    use crate::services::family::family_store::MEMBER_DEVICE_LIMIT;
    let f = fam().await;
    let installs = MEMBER_DEVICE_LIMIT + 20;
    for n in 0..installs {
        let headers = vec![("x-octo-device-id", format!("install-{n}"))];
        let uri = format!("/rest/getFamily?u=alex&p={ALEX_PASSWORD}&v=1.16.1&c=Octo&f=json");
        let r = raw(&f.app, Method::GET, &uri, &headers, String::new(), HOME).await;
        assert!(ok(&r), "{}", r.body);
    }
    let kept = f.state.family_store.devices_of("alex");
    assert!(kept.len() <= MEMBER_DEVICE_LIMIT, "{}", kept.len());
    let newest = detected_device_id(
        "alex",
        &Sighting {
            octo_device_id: Some(format!("install-{}", installs - 1)),
            ..Default::default()
        },
    );
    assert!(kept.iter().any(|d| d.id == newest), "the newest is kept");
    // Other members keep theirs.
    assert!(f.state.family_store.device("d_sam").is_some());
}

// ------------------------------------------------------------------------------------------
// Signing in on another device with a QR code
// ------------------------------------------------------------------------------------------

/// No route takes the key: a key sent anyway is never kept or answered, and every log line
/// masks the hand-over token, the redemption id, the key and the box.
#[tokio::test]
async fn no_route_takes_the_hand_over_key_and_logs_mask_every_hand_over_secret() {
    let f = fam().await;
    let (capture, _guard) = LogCapture::start();
    let key = "KeyKeyKey0123456789abcdefKeyKeyKey0123456789a";
    // Through the Family page's API.
    let token = token_of(&start_hand_over(&f, &f.alex()).await);
    let redeemed = f
        .call(
            Method::POST,
            &format!("/api/family/signin/redeem?k={key}"),
            None,
            Some(json!({ "token": token, "deviceName": "Pixel", "platform": "Android", "k": key })),
            AWAY,
        )
        .await;
    assert_eq!(redeemed.status, StatusCode::OK, "{}", redeemed.body);
    let id = redeemed.json()["id"].as_str().expect("an id").to_string();
    let mut replies = vec![redeemed];
    replies.push(
        f.get(&format!("/api/family/signin/{token}/pending"), Some(&f.alex()))
            .await,
    );
    replies.push(
        f.post(
            &format!("/api/family/signin/{token}/decide"),
            Some(&f.alex()),
            json!({ "allow": true }),
        )
        .await,
    );
    replies.push(
        f.post(
            &format!("/api/family/signin/{token}/box"),
            Some(&f.alex()),
            json!({ "box": "OpaqueSealedBox1" }),
        )
        .await,
    );
    let fetched = f
        .get(&format!("/api/family/signin/redeem/{id}?k={key}"), None)
        .await;
    assert_eq!(fetched.json()["box"], "OpaqueSealedBox1");
    replies.push(fetched);
    // Through the apps' calls.
    let start = f.rest_as_alex("startFamilySignIn", "").await.subsonic();
    let rest_token = start["familySignIn"]["token"]
        .as_str()
        .expect("a token")
        .to_string();
    let redeemed = rest_query(
        &f,
        "redeemFamilySignIn",
        &format!("token={rest_token}&deviceName=Pixel&platform=Android&k={key}"),
        AWAY,
    )
    .await;
    let rest_id = redeemed.subsonic()["familySignInRedeemed"]["id"]
        .as_str()
        .expect("an id")
        .to_string();
    replies.push(redeemed);
    replies.push(
        f.rest_as_alex("decideFamilySignIn", &format!("token={rest_token}&allow=true"))
            .await,
    );
    replies.push(
        f.rest_as_alex(
            "putFamilySignInBox",
            &format!("token={rest_token}&box=OpaqueSealedBox2"),
        )
        .await,
    );
    let fetched = rest_query(
        &f,
        "getFamilySignInRedeem",
        &format!("id={rest_id}&k={key}"),
        AWAY,
    )
    .await;
    assert_eq!(
        fetched.subsonic()["familySignInRedeem"]["box"],
        "OpaqueSealedBox2"
    );
    replies.push(fetched);
    for reply in &replies {
        assert!(!everything(reply).contains(key), "{}", reply.body);
    }
    let lines = written(&capture);
    assert!(
        lines.iter().any(|l| l.contains("/api/family/signin/")),
        "{lines:?}"
    );
    for secret in [
        token.as_str(),
        id.as_str(),
        rest_token.as_str(),
        rest_id.as_str(),
        key,
        "OpaqueSealedBox1",
        "OpaqueSealedBox2",
    ] {
        for line in &lines {
            assert!(!line.contains(secret), "{secret} logged: {line}");
        }
    }
}

/// A box is taken only from the account that started the hand-over, only after it allowed the
/// new device, only once, and only up to 16 KB, on both APIs.
#[tokio::test]
async fn a_box_is_taken_only_from_the_starter_after_allow_once_and_at_most_sixteen_kilobytes() {
    use crate::services::family::handover::MAX_BOX;
    let f = fam().await;
    let token = token_of(&start_hand_over(&f, &f.alex()).await);
    let id = redeem_from(&f, &token, AWAY).await.json()["id"]
        .as_str()
        .expect("an id")
        .to_string();
    let put = format!("/api/family/signin/{token}/box");
    let decide = format!("/api/family/signin/{token}/decide");
    // Before allow: nobody's box is taken, the starter's included.
    for cookie in [f.alex(), f.sam(), f.owner_cookie()] {
        let r = f.post(&put, Some(&cookie), json!({ "box": "early" })).await;
        assert_ne!(r.status, StatusCode::NO_CONTENT, "{}", r.body);
    }
    assert!(!ok(&f
        .rest_as_sam("putFamilySignInBox", &format!("token={token}&box=x"))
        .await));
    // Another account cannot allow it either.
    for cookie in [f.sam(), f.owner_cookie()] {
        assert_eq!(
            f.post(&decide, Some(&cookie), json!({ "allow": true }))
                .await
                .status,
            StatusCode::NOT_FOUND
        );
    }
    assert_eq!(fetch_box(&f, &id).await, json!({ "state": "Waiting" }));
    f.post(&decide, Some(&f.alex()), json!({ "allow": true })).await;
    // After allow: still only the starter.
    for cookie in [f.sam(), f.owner_cookie()] {
        assert_eq!(
            f.post(&put, Some(&cookie), json!({ "box": "not theirs" }))
                .await
                .status,
            StatusCode::NOT_FOUND
        );
    }
    assert!(!ok(&f
        .rest_as_owner("putFamilySignInBox", &format!("token={token}&box=x"))
        .await));
    let over = "x".repeat(MAX_BOX + 1);
    assert_eq!(
        f.post(&put, Some(&f.alex()), json!({ "box": over })).await.status,
        StatusCode::BAD_REQUEST
    );
    let r = rest_form(
        &f,
        "putFamilySignInBox",
        &format!("u=alex&p={ALEX_PASSWORD}&token={token}&box={over}"),
        HOME,
    )
    .await;
    assert!(!ok(&r), "a box over 16 KB through the apps' call");
    let largest = "y".repeat(MAX_BOX);
    let r = f.post(&put, Some(&f.alex()), json!({ "box": largest })).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.body);
    // Once: a second box is refused while the first waits, and after it is fetched.
    assert_ne!(
        f.post(&put, Some(&f.alex()), json!({ "box": "second" }))
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(fetch_box(&f, &id).await["box"], json!(largest));
    assert_ne!(
        f.post(&put, Some(&f.alex()), json!({ "box": "third" }))
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(fetch_box(&f, &id).await, json!({ "state": "Expired" }));
}

/// The box goes to the redemption it was made for, and only once: no other redemption's id,
/// made-up id or the token itself fetches it.
#[tokio::test]
async fn a_box_goes_only_to_its_own_redemption_and_only_once() {
    let f = fam().await;
    let first = token_of(&start_hand_over(&f, &f.alex()).await);
    let second = token_of(&start_hand_over(&f, &f.alex()).await);
    let id = redeem_from(&f, &first, AWAY).await.json()["id"]
        .as_str()
        .expect("an id")
        .to_string();
    let other = redeem_from(&f, &second, AWAY).await.json()["id"]
        .as_str()
        .expect("an id")
        .to_string();
    f.post(
        &format!("/api/family/signin/{first}/decide"),
        Some(&f.alex()),
        json!({ "allow": true }),
    )
    .await;
    f.post(
        &format!("/api/family/signin/{first}/box"),
        Some(&f.alex()),
        json!({ "box": "for-the-first" }),
    )
    .await;
    for wrong in [
        other.as_str(),
        first.as_str(),
        "made-up",
        &id[..id.len() - 1],
        &format!("{id}x"),
        &id.to_uppercase(),
    ] {
        let fetched = fetch_box(&f, wrong).await;
        assert!(fetched.get("box").is_none(), "{wrong}: {fetched}");
    }
    assert_eq!(fetch_box(&f, &other).await, json!({ "state": "Waiting" }));
    assert_eq!(fetch_box(&f, &id).await["box"], "for-the-first");
    assert_eq!(fetch_box(&f, &id).await, json!({ "state": "Expired" }));
}

/// A token works once; wrong ones count against the peer, and redemptions against the account
/// whatever peers send them.
#[tokio::test]
async fn hand_over_tokens_are_single_use_and_throttled_per_peer_and_account() {
    use crate::services::family::handover::{ACCOUNT_REDEEMS, PEER_FAILURES};
    let f = fam().await;
    let token = token_of(&start_hand_over(&f, &f.alex()).await);
    assert_eq!(redeem_from(&f, &token, AWAY).await.status, StatusCode::OK);
    for from in [AWAY, HOME, "10.70.0.1"] {
        assert_eq!(
            redeem_from(&f, &token, from).await.status,
            StatusCode::FORBIDDEN,
            "a used token"
        );
    }
    // Wrong tokens from one peer, through either API, until it waits.
    let peer = "10.71.0.1";
    for n in 0..PEER_FAILURES {
        let r = if n % 2 == 0 {
            redeem_from(&f, &format!("wrong-{n}"), peer).await.status
        } else {
            let r = rest_query(&f, "redeemFamilySignIn", &format!("token=wrong-{n}"), peer).await;
            assert_eq!(code(&r), 40);
            StatusCode::FORBIDDEN
        };
        assert_eq!(r, StatusCode::FORBIDDEN);
    }
    let fresh = token_of(&start_hand_over(&f, &f.sam()).await);
    assert_eq!(
        redeem_from(&f, &fresh, peer).await.status,
        StatusCode::TOO_MANY_REQUESTS
    );
    // One account's redemptions are bounded, from however many peers.
    let mut redeemed = 1;
    for n in 0..(ACCOUNT_REDEEMS + 3) {
        let token = token_of(&start_hand_over(&f, &f.alex()).await);
        if redeem_from(&f, &token, &format!("10.72.0.{}", n + 1))
            .await
            .status
            == StatusCode::OK
        {
            redeemed += 1;
        }
    }
    assert_eq!(redeemed, ACCOUNT_REDEEMS);
}

/// Redeeming someone else's token gets nothing until they allow it, and a denied or expired
/// one never gets anything.
#[tokio::test]
async fn redeeming_someone_elses_token_without_their_allow_yields_nothing() {
    let f = fam().await;
    let token = token_of(&start_hand_over(&f, &f.alex()).await);
    let thief = redeem_from(&f, &token, "198.51.100.66").await.json()["id"]
        .as_str()
        .expect("an id")
        .to_string();
    for _ in 0..3 {
        assert_eq!(fetch_box(&f, &thief).await, json!({ "state": "Waiting" }));
    }
    let r = f
        .post(
            &format!("/api/family/signin/{token}/decide"),
            Some(&f.alex()),
            json!({ "allow": false }),
        )
        .await;
    assert_eq!(r.json(), json!({ "state": "Denied" }));
    // A box is refused after a deny.
    assert_eq!(
        f.post(
            &format!("/api/family/signin/{token}/box"),
            Some(&f.alex()),
            json!({ "box": "never" })
        )
        .await
        .status,
        StatusCode::CONFLICT
    );
    assert_eq!(fetch_box(&f, &thief).await, json!({ "state": "Denied" }));
    assert_eq!(fetch_box(&f, &thief).await, json!({ "state": "Expired" }));
    assert_eq!(f.state.family.handover.held(), 0, "a deny leaves nothing behind");
}

/// Many new devices redeeming one token at once: one waits for an answer, every other one is
/// refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn many_redemptions_of_one_token_at_once_leave_exactly_one_waiting() {
    let f = fam().await;
    let token = token_of(&start_hand_over(&f, &f.alex()).await);
    let tries = (0..40).map(|n| {
        let (app, token) = (f.app.clone(), token.clone());
        tokio::spawn(async move {
            raw(
                &app,
                Method::POST,
                "/api/family/signin/redeem",
                &[
                    ("content-type", "application/json".to_string()),
                    ("x-octo-family", "1".to_string()),
                ],
                json!({ "token": token, "deviceName": format!("Device {n}"), "platform": "Android" })
                    .to_string(),
                &format!("10.80.0.{}", n + 1),
            )
            .await
        })
    });
    let replies = futures::future::join_all(tries).await;
    let statuses: Vec<StatusCode> = replies.into_iter().map(|r| r.expect("ran").status).collect();
    assert_eq!(
        statuses.iter().filter(|s| **s == StatusCode::OK).count(),
        1,
        "{statuses:?}"
    );
    assert!(
        statuses
            .iter()
            .all(|s| *s == StatusCode::OK || *s == StatusCode::FORBIDDEN)
    );
    let pending = f
        .get(&format!("/api/family/signin/{token}/pending"), Some(&f.alex()))
        .await;
    assert_eq!(pending.status, StatusCode::OK);
}

/// Starting a hand-over needs a sign-in that checked out, and works for the owner and for
/// accounts outside the family as for members.
#[tokio::test]
async fn starting_a_hand_over_needs_a_checked_sign_in_and_works_for_the_owner_and_unmanaged_accounts() {
    let f = fam().await;
    only_real_sign_ins(&f).await;
    // The Family page, signed in through Navidrome.
    for (user, password) in [("winters", OWNER_PASSWORD), ("bob", BOB_PASSWORD)] {
        let signed = f
            .post(
                "/api/family/auth",
                None,
                json!({ "username": user, "password": password }),
            )
            .await;
        assert_eq!(signed.status, StatusCode::OK, "{user}: {}", signed.body);
        let cookie = format!("octo_family={}", signed.cookie_token());
        let start = start_hand_over(&f, &cookie).await;
        let token = token_of(&start);
        assert!(
            start["links"]["home"]
                .as_str()
                .is_some_and(|l| l.ends_with("/family/signin"))
        );
        let r = f
            .get(&format!("/api/family/signin/{token}/pending"), Some(&cookie))
            .await;
        assert_eq!(r.status, StatusCode::NO_CONTENT, "{user}");
        // The apps' call and a signed Family call.
        let r = rest_query(
            &f,
            "startFamilySignIn",
            &format!("u={user}&p={}", esc(password)),
            HOME,
        )
        .await;
        assert!(ok(&r), "{user}: {}", r.body);
        let r = raw(
            &f.app,
            Method::POST,
            &format!(
                "/api/family/signin/start?u={user}&p={}&v=1.16.1&c=test",
                esc(password)
            ),
            &[],
            String::new(),
            HOME,
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{user}: {}", r.body);
        // A wrong password starts nothing.
        let r = rest_query(
            &f,
            "startFamilySignIn",
            &format!("u={user}&p=wrong-password"),
            HOME,
        )
        .await;
        assert_eq!(code(&r), 40, "{user}: {}", r.body);
        let r = raw(
            &f.app,
            Method::POST,
            &format!("/api/family/signin/start?u={user}&p=wrong-password&v=1.16.1&c=test"),
            &[],
            String::new(),
            HOME,
        )
        .await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{user}: {}", r.body);
    }
    // Nobody at all.
    assert_eq!(
        f.post("/api/family/signin/start", None, json!({})).await.status,
        StatusCode::UNAUTHORIZED
    );
    let r = raw(
        &f.app,
        Method::GET,
        "/rest/startFamilySignIn?v=1.16.1&c=test&f=json",
        &[],
        String::new(),
        HOME,
    )
    .await;
    assert!(!ok(&r), "{}", r.body);
}

/// However many hand-overs one account starts, another account's hand-over stays.
#[tokio::test]
async fn one_account_starting_many_hand_overs_never_pushes_out_another_accounts() {
    let f = fam().await;
    let sams = token_of(&start_hand_over(&f, &f.sam()).await);
    let cookie = f.alex();
    for _ in 0..2_100 {
        f.state.family.handover.start("alex");
    }
    start_hand_over(&f, &cookie).await;
    let r = f
        .get(&format!("/api/family/signin/{sams}/pending"), Some(&f.sam()))
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.body);
    assert_eq!(redeem_from(&f, &sams, AWAY).await.status, StatusCode::OK);
}

// ------------------------------------------------------------------------------------------
// Routes that are gone
// ------------------------------------------------------------------------------------------

/// Pairing, app passwords and adding devices are gone from both APIs: nothing answers them as
/// Octo, and nothing changes.
#[tokio::test]
async fn the_pairing_and_app_password_routes_are_gone() {
    let f = fam().await;
    let devices = f.state.family_store.devices();
    for call in [
        "octoFamilyPair?username=alex&code=123456&deviceName=x",
        "addFamilyDevice?name=x&kind=SubsonicApp",
        "addFamilyDevice?name=x&kind=OctoApp&username=alex",
        "signOutFamilyDevice?id=d_app",
    ] {
        for sign_in in [
            String::new(),
            format!("u=alex&p={ALEX_PASSWORD}&"),
            format!("u=sam&p={SAM_PASSWORD}&"),
            format!("u=winters&p={}&", esc(OWNER_PASSWORD)),
        ] {
            let (endpoint, query) = call.split_once('?').expect("a query");
            let uri = format!("/rest/{endpoint}?{sign_in}{query}&v=1.16.1&c=test&f=json");
            let r = raw(&f.app, Method::GET, &uri, &[], String::new(), HOME).await;
            assert!(!ok(&r), "{uri}: {}", r.body);
            assert!(!r.body.contains("\"type\":\"octo\""), "{uri}: {}", r.body);
            for word in ["password", "secret", "familyDevice", "familyPair"] {
                assert!(!r.body.contains(word), "{uri}: {}", r.body);
            }
        }
    }
    for (verb, uri) in [
        (Method::POST, "/api/family/pair"),
        (Method::POST, "/api/family/me/devices"),
        (Method::POST, "/api/family/members/alex/devices"),
        (Method::POST, "/api/family/me/devices/d_app/signout"),
    ] {
        for cookie in [None, Some(f.alex()), Some(f.sam())] {
            let r = f
                .call(
                    verb.clone(),
                    uri,
                    cookie.as_deref(),
                    Some(json!({ "username": "alex", "code": "123456", "name": "x", "kind": "SubsonicApp" })),
                    HOME,
                )
                .await;
            assert_eq!(r.status, StatusCode::NOT_FOUND, "{verb} {uri}: {}", r.body);
        }
    }
    // Every device is still there, and the only new ones are the apps that signed in with the
    // right password; none was made or named by the calls themselves.
    let after = f.state.family_store.devices();
    for device in &devices {
        assert!(after.iter().any(|d| d.id == device.id), "{} went", device.id);
    }
    for device in after.iter().filter(|d| !devices.contains(d)) {
        let expected = app_id(&device.username, "test");
        assert!(
            device.id == expected || devices.iter().any(|d| d.id == device.id),
            "{device:?}"
        );
        assert_ne!(device.name, "x");
    }
}

/// A `family.json` from before one login, with device secrets and app passwords sealed in it:
/// none of them signs anything in, anywhere, and they are gone at the next write.
#[tokio::test]
async fn old_stored_device_secrets_sign_nothing_in() {
    let f = fam().await;
    let secret = "old-device-secret-1";
    let sealed = f.state.family_vault.seal(secret).expect("sealed");
    // Written as an older Octo kept it: alex still has a password; sam signed up with a hash.
    let mut file: Value =
        serde_json::from_str(&serde_json::to_string(&f.state.family_store.snapshot()).expect("written"))
            .expect("json");
    for device in file["Devices"].as_array_mut().expect("devices") {
        device["Kind"] = json!("SubsonicApp");
        device["Secret"] = json!(sealed);
        device["AppPassword"] = json!(sealed);
    }
    for member in file["Members"].as_array_mut().expect("members") {
        if member["Username"] == "sam" {
            member["AccountPassword"] = json!("$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA");
        }
    }
    file["PairCodes"] = json!([{ "CodeHash": "x", "Username": "alex", "DeviceName": "", "Expires": "2099-01-01T00:00:00Z", "Tries": 0 }]);
    std::fs::write(f._dir.path().join("family.json"), file.to_string()).expect("written");
    let old = AppState::for_tests_with(
        AppSettings {
            family: FamilySettings {
                enabled: true,
                ..Default::default()
            },
            subsonic: SubsonicSettings {
                url: Some(f.nd.uri()),
                ..Default::default()
            },
            ..Default::default()
        },
        TestServices {
            config_dir: Some(f._dir.path().to_path_buf()),
            ..Default::default()
        },
    );
    let app = build_with(
        old.clone(),
        app_routes(&StaticAssets::load(&StaticRoots::from_env())),
    );
    assert!(old.family_store.device("d_app").is_some());
    for user in ["alex", "sam"] {
        for sign_in in [
            format!("u={user}&p={secret}"),
            format!("u={user}&p=enc:{}", hex::encode(secret)),
            format!("u={user}&t={}&s=ab12", subsonic_token(secret, "ab12")),
        ] {
            let uri = format!("/rest/getFamily?{sign_in}&v=1.16.1&c=test&f=json");
            let r = raw(&app, Method::GET, &uri, &[], String::new(), HOME).await;
            assert_eq!(code(&r), 40, "{uri}: {}", r.body);
            let uri = format!("/api/family/me?{sign_in}&v=1.16.1&c=test");
            let r = raw(&app, Method::GET, &uri, &[], String::new(), HOME).await;
            assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{uri}: {}", r.body);
        }
        let r = raw(
            &app,
            Method::POST,
            "/api/family/auth",
            &[
                ("content-type", "application/json".to_string()),
                ("x-octo-family", "1".to_string()),
            ],
            json!({ "username": user, "password": secret }).to_string(),
            HOME,
        )
        .await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{user}: {}", r.body);
        let r = web_login(&app, user, secret, HOME).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{user}: {}", r.body);
    }
    // Alex's own password still works; the old secrets are gone at the next write.
    let uri = format!("/rest/getFamily?u=alex&p={ALEX_PASSWORD}&v=1.16.1&c=test&f=json");
    assert!(ok(&raw(&app, Method::GET, &uri, &[], String::new(), HOME).await));
    old.family_store.set_owner("winters");
    let written = std::fs::read_to_string(f._dir.path().join("family.json")).expect("written");
    for gone in ["Secret", "AppPassword", "PairCodes", &sealed] {
        assert!(!written.contains(gone), "{gone} in {written}");
    }
}

// ------------------------------------------------------------------------------------------
// Family off
// ------------------------------------------------------------------------------------------

/// With Family off, every new route answers that Family is off, the apps' new calls go to
/// Navidrome as any unknown call would, and no hand-over is made.
#[tokio::test]
async fn with_family_off_the_new_routes_answer_family_off_and_make_nothing() {
    let f = fam_with(|s| s.family.enabled = false, None).await;
    let owner = f.owner_cookie();
    let signed = format!("u=winters&p={}&v=1.16.1&c=test", esc(OWNER_PASSWORD));
    let api = [
        (
            Method::POST,
            "/api/family/signin/start".to_string(),
            Some(json!({})),
        ),
        (
            Method::POST,
            "/api/family/signin/redeem".to_string(),
            Some(json!({ "token": "t", "deviceName": "x", "platform": "y" })),
        ),
        (Method::GET, "/api/family/signin/redeem/some-id".to_string(), None),
        (
            Method::GET,
            "/api/family/signin/some-token/pending".to_string(),
            None,
        ),
        (
            Method::POST,
            "/api/family/signin/some-token/decide".to_string(),
            Some(json!({ "allow": true })),
        ),
        (
            Method::POST,
            "/api/family/signin/some-token/box".to_string(),
            Some(json!({ "box": "x" })),
        ),
        (Method::GET, "/api/family/me/login".to_string(), None),
        (
            Method::PUT,
            "/api/family/me/password".to_string(),
            Some(json!({ "current": "a", "next": "bbbbbbbb" })),
        ),
        (
            Method::POST,
            "/api/family/members/alex/reset".to_string(),
            Some(json!({})),
        ),
    ];
    for (verb, uri, body) in api {
        for (cookie, query) in [(Some(owner.as_str()), String::new()), (None, signed.clone())] {
            let uri = if query.is_empty() {
                uri.clone()
            } else {
                format!("{uri}?{query}")
            };
            let r = f.call(verb.clone(), &uri, cookie, body.clone(), HOME).await;
            assert_eq!(r.status, StatusCode::NOT_FOUND, "{verb} {uri}: {}", r.body);
            assert_eq!(r.error(), "Family is off on this server.", "{verb} {uri}");
        }
    }
    for call in [
        "getFamilyLogin",
        "changeFamilyPassword&current=a&next=bbbbbbbb",
        "startFamilySignIn",
        "redeemFamilySignIn&token=t",
        "getFamilySignInPending&token=t",
        "decideFamilySignIn&token=t&allow=true",
        "putFamilySignInBox&token=t&box=x",
        "getFamilySignInRedeem&id=x",
    ] {
        let (endpoint, rest) = call.split_once('&').unwrap_or((call, ""));
        let r = f.rest_as_owner(endpoint, rest).await;
        assert!(!r.body.contains("\"type\":\"octo\""), "{call}: {}", r.body);
        let relayed = f.nd.received_requests().await.unwrap_or_default();
        assert!(
            relayed
                .iter()
                .any(|r| r.url.path() == format!("/rest/{endpoint}")),
            "{call} reached Navidrome"
        );
    }
    assert_eq!(f.state.family.handover.held(), 0);
    let alex = f.state.family_store.member("alex").expect("alex");
    assert!(passwords::matches(
        &f.state.family_vault,
        &alex.account_password,
        ALEX_PASSWORD
    ));
}

// ------------------------------------------------------------------------------------------
// The page a hand-over QR code opens
// ------------------------------------------------------------------------------------------

fn family_pages() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../octo/wwwroot/family")
}

fn page_source(name: &str) -> String {
    let path = family_pages().join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The sign-in page reads the fragment and sends none of it anywhere: no network call of its
/// own, the address bar cleared once read, and no referrer for the shared ones.
#[tokio::test]
async fn the_sign_in_page_sends_nothing_from_its_fragment() {
    let f = fam().await;
    let page = f.get("/family/signin", None).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(
        page.body
            .contains(r#"<meta name="referrer" content="no-referrer" />"#)
    );
    assert!(
        page.body
            .contains(r#"<script src="/family/signin.js" defer></script>"#)
    );
    assert!(!page.body.contains("<script>"), "no inline script");
    let script = f.get("/family/signin.js", None).await.body;
    assert_eq!(script, page_source("signin.js"));
    for network in [
        "fetch(",
        "F.api(",
        "sendBeacon",
        "XMLHttpRequest",
        "WebSocket",
        "EventSource",
        "new Image",
        ".src",
        "window.open",
        "postMessage",
        "importScripts",
        "document.cookie",
        "localStorage",
        "sessionStorage",
    ] {
        assert!(!script.contains(network), "signin.js uses {network}");
    }
    assert!(script.contains("history.replaceState(null, '', location.pathname)"));
    // The shared script's own calls never carry the address.
    let common = f.get("/family/common.js", None).await.body;
    let calls: Vec<&str> = common
        .match_indices("fetch(")
        .map(|(at, _)| &common[at..])
        .collect();
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert!(calls[0].starts_with("fetch('/api/family' + path, options)"));
    assert!(calls[1].starts_with("fetch('/admin/icons.svg')"));
    for network in ["sendBeacon", "XMLHttpRequest", "location.hash", "location.href"] {
        assert!(!common.contains(network), "common.js uses {network}");
    }
}

/// The page, run with a stand-in browser: on Android it opens Octo at once through an intent
/// link carrying the whole hand-over (and coming back here with `back=1`), on a computer it
/// offers an `octo://handover` link, and nothing it asks the network for carries the token or
/// the key. Skipped where Node.js is not installed.
#[test]
fn the_sign_in_page_hands_over_through_intent_and_octo_links() {
    let node = std::process::Command::new("node").arg("--version").output();
    if !node.is_ok_and(|out| out.status.success()) {
        eprintln!("node is not installed; the sign-in page's run is skipped");
        return;
    }
    let dir = tempfile::tempdir().expect("a temp dir");
    let harness = dir.path().join("harness.js");
    std::fs::write(&harness, SIGN_IN_HARNESS).expect("written");
    let family = family_pages();
    let token = "Tok_en-123";
    let key = "KeyAbc_-987";
    let fragment = format!(
        "t={token}&k={key}&s=https%3A%2F%2Fmusic.example.com&h=http%3A%2F%2F192.168.1.20%3A5274&u=alex"
    );
    let run = |browser: &str, fragment: &str| -> Value {
        let out = std::process::Command::new("node")
            .arg(&harness)
            .arg(family.join("common.js"))
            .arg(family.join("signin.js"))
            .arg(browser)
            .arg(fragment)
            .output()
            .expect("node ran");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        serde_json::from_slice(&out.stdout).expect("the harness answers JSON")
    };
    let android = run(
        "Mozilla/5.0 (Linux; Android 14; Pixel 9) AppleWebKit/537.36 Chrome/129 Mobile",
        &fragment,
    );
    let desktop = run(
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/129",
        &fragment,
    );
    let back = run(
        "Mozilla/5.0 (Linux; Android 14; Pixel 9) AppleWebKit/537.36 Chrome/129 Mobile",
        &format!("{fragment}&back=1"),
    );
    for result in [&android, &desktop, &back] {
        for asked in result["network"].as_array().expect("a list") {
            let asked = asked.as_str().expect("text");
            assert!(!asked.contains(token) && !asked.contains(key), "{asked}");
        }
        assert_eq!(result["replaced"], json!(["/family/signin"]), "{result}");
        assert!(!result["text"].as_str().expect("text").contains(key), "{result}");
    }
    let query = "t=Tok_en-123&k=KeyAbc_-987&s=https%3A%2F%2Fmusic.example.com&h=http%3A%2F%2F192.168.1.20%3A5274&u=alex";
    let fallback = format!("http://192.168.1.20:5274/family/signin#{query}&back=1");
    let intent = format!(
        "intent://handover?{query}#Intent;scheme=octo;package=app.winters.octo;S.browser_fallback_url={};end",
        url::form_urlencoded::byte_serialize(fallback.as_bytes())
            .collect::<String>()
            .replace('+', "%20")
    );
    assert_eq!(android["navigated"], json!([intent]), "{android}");
    assert!(
        android["links"]
            .as_array()
            .expect("links")
            .contains(&json!(intent))
    );
    assert_eq!(desktop["navigated"], json!([]), "{desktop}");
    assert!(
        desktop["links"]
            .as_array()
            .expect("links")
            .contains(&json!(format!("octo://handover?{query}"))),
        "{desktop}"
    );
    // Back from an intent that found no app: the page stays, with the same link to press.
    assert_eq!(back["navigated"], json!([]), "{back}");
    assert!(back["links"].as_array().expect("links").contains(&json!(intent)));
    assert!(
        back["text"]
            .as_str()
            .expect("text")
            .contains("may not be installed")
    );
    // Without the key, nothing is handed over.
    let partial = run(
        "Mozilla/5.0 (Linux; Android 14; Pixel 9) AppleWebKit/537.36 Chrome/129 Mobile",
        &format!("t={token}"),
    );
    assert_eq!(partial["navigated"], json!([]));
    assert!(partial["text"].as_str().expect("text").contains("not complete"));
}

/// A stand-in browser for `signin.js`: just enough of the page to run it, recording every
/// network call, navigation, address change, link and text.
const SIGN_IN_HARNESS: &str = r#"
'use strict';
const fs = require('fs');
const vm = require('vm');
const [common, signin, browser, fragment] = process.argv.slice(2);
const network = [], navigated = [], replaced = [], links = [];
function node(tag) {
  const n = {
    tagName: String(tag).toUpperCase(), children: [], attributes: {}, className: '', style: {},
    _text: '', parentNode: null, open: false,
    classList: { add() {}, remove() {}, toggle() {}, contains() { return false; } },
    appendChild(c) { this.children.push(c); c.parentNode = this; return c; },
    removeChild(c) { this.children = this.children.filter((x) => x !== c); return c; },
    remove() {}, get firstChild() { return this.children[0] || null; },
    setAttribute(k, v) { this.attributes[k] = String(v); if (k === 'href') links.push(String(v)); if (k === 'src') network.push(String(v)); },
    getAttribute(k) { return this.attributes[k]; },
    hasAttribute(k) { return k in this.attributes; },
    removeAttribute(k) { delete this.attributes[k]; },
    addEventListener() {}, removeEventListener() {}, focus() {}, insertAdjacentHTML() {},
    showModal() {}, close() {}, scrollBy() {}, closest() { return null; },
    querySelector(sel) { return find(this, (x) => x.tagName === sel.toUpperCase()); },
    querySelectorAll() { return []; },
    get textContent() { return this._text + this.children.map((c) => c.textContent).join(''); },
    set textContent(v) { this.children = []; this._text = String(v); },
  };
  return n;
}
function find(root, test) {
  for (const c of root.children) { if (test(c)) return c; const f = find(c, test); if (f) return f; }
  return null;
}
const ids = {};
const document = {
  body: node('body'),
  createElement: node, createElementNS: (_, tag) => node(tag),
  createTextNode: (t) => { const n = node('#text'); n._text = String(t); return n; },
  getElementById: (id) => (ids[id] = ids[id] || node('div')),
  addEventListener() {}, execCommand() {}, cookie: '',
};
const location = {
  protocol: 'http:', host: '192.168.1.20:5274', origin: 'http://192.168.1.20:5274',
  pathname: '/family/signin', search: '', hash: '#' + fragment,
};
Object.defineProperty(location, 'href', {
  get() { return this.origin + this.pathname + this.hash; },
  set(v) { navigated.push(String(v)); },
});
const history = { replaceState(_, __, url) { replaced.push(String(url)); location.hash = ''; } };
const navigator = {
  userAgent: browser, maxTouchPoints: 0,
  sendBeacon(url, body) { network.push(String(url) + ' ' + String(body)); return true; },
  clipboard: { writeText: async () => {} },
};
function fetch(url, options) {
  network.push(String(url) + ' ' + JSON.stringify(options || {}));
  return Promise.resolve({ ok: false, status: 404, text: async () => '' });
}
function XMLHttpRequest() { this.open = (m, u) => network.push(String(u)); this.send = () => {}; this.setRequestHeader = () => {}; }
function Image() { const i = node('img'); return i; }
const window = {
  document, location, history, navigator, fetch, XMLHttpRequest, Image,
  matchMedia: () => ({ matches: false }), addEventListener() {}, removeEventListener() {},
};
window.window = window;
const context = vm.createContext(Object.assign(window, {
  URLSearchParams, URL, console, setTimeout, clearTimeout, Promise, JSON, encodeURIComponent,
  decodeURIComponent, MutationObserver: function () { this.observe = () => {}; this.disconnect = () => {}; },
  requestAnimationFrame: () => 0,
}));
vm.runInContext(fs.readFileSync(common, 'utf8'), context, { filename: common });
vm.runInContext(fs.readFileSync(signin, 'utf8'), context, { filename: signin });
setTimeout(() => {
  process.stdout.write(JSON.stringify({
    network, navigated, replaced, links, text: ids.signin ? ids.signin.textContent : '',
  }));
}, 20);
"#;
