//! A member's own imports on the Family page: where to export a library to a file, a file to
//! upload, a list to paste, and the lists already there. Members with the import ability only;
//! the owner imports from the dashboard.

use axum::body::Bytes;
use axum::extract::{FromRequest, Request, State};
use axum::http::{StatusCode, header};
use axum::response::Response;
use serde::Deserialize;
use serde_json::json;

use super::{Web, body_as, ok, refusal};
use crate::app::AppState;
use crate::services::family::Member;
use crate::services::family::family_service::Actor;
use crate::services::family::rules::IMPORTS_OFF;
use crate::services::imports::ImportActionResult;
use crate::services::imports::import_file_reader::ImportFileReader;

/// The largest upload: the file reader's own limit, and room for the form around it.
pub const FILE_BODY: usize = ImportFileReader::MAX_BYTES as usize + 64 * 1024;

/// The member signed in, when they may import.
async fn importer(state: &AppState, web: &Web) -> Result<(Actor, Member), Response> {
    let (signed, member) = web.member(state).await?;
    if !member.abilities().import_playlists {
        return Err(refusal(StatusCode::FORBIDDEN, IMPORTS_OFF));
    }
    Ok((signed.actor, member))
}

/// An import's result: 200 when it worked, 400 with its words when it did not.
fn result(result: &ImportActionResult) -> Response {
    let status = if result.ok {
        StatusCode::OK
    } else {
        StatusCode::BAD_REQUEST
    };
    super::answer(status, &json!(result))
}

/// `GET /api/family/me/imports`: the export services, whether downloads wait for approval,
/// and the member's lists.
pub async fn my_imports(State(state): State<AppState>, web: Web) -> Response {
    let (actor, _) = match importer(&state, &web).await {
        Ok(found) => found,
        Err(res) => return res,
    };
    let overview = state.imports.overview(&actor.user);
    let services = state.imports.services();
    ok(&json!({
        "services": services.services,
        "approval": overview.approval,
        "lists": overview.lists,
        "libraryProblem": overview.library_problem,
    }))
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct TextBody {
    pub name: Option<String>,
    pub text: String,
}

/// `POST /api/family/me/imports/text {name, text}`: a pasted list.
pub async fn import_text(State(state): State<AppState>, web: Web, body: Bytes) -> Response {
    let (actor, _) = match importer(&state, &web).await {
        Ok(found) => found,
        Err(res) => return res,
    };
    let request: TextBody = match body_as(&body) {
        Ok(request) => request,
        Err(res) => return res,
    };
    if request.text.trim().is_empty() {
        return result(&ImportActionResult::failed("Paste a list first."));
    }
    let imports = state.imports.clone();
    let user = actor.user;
    // Not tied to the request: a closed tab must not leave a list half matched.
    match tokio::spawn(async move {
        imports
            .add_text(&user, request.name.as_deref(), &request.text)
            .await
    })
    .await
    {
        Ok(done) => result(&done),
        Err(_) => refusal(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Octo could not read that list.",
        ),
    }
}

/// `POST /api/family/me/imports/file`: a multipart form with the file in `file`.
pub async fn import_file(State(state): State<AppState>, web: Web, request: Request) -> Response {
    let (actor, _) = match importer(&state, &web).await {
        Ok(found) => found,
        Err(res) => return res,
    };
    let choose = || result(&ImportActionResult::failed("Choose a file first."));
    let multipart = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .eq_ignore_ascii_case("multipart/form-data")
        });
    if !multipart {
        return choose();
    }
    let Ok(mut form) = axum::extract::Multipart::from_request(request, &state).await else {
        return choose();
    };
    let mut found: Option<(String, Vec<u8>)> = None;
    loop {
        let field = match form.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => break,
            Err(_) => {
                return refusal(
                    StatusCode::BAD_REQUEST,
                    "That file did not arrive whole. Files up to 64 MB work; try again.",
                );
            }
        };
        if found.is_some() || !field.name().is_some_and(|n| n.eq_ignore_ascii_case("file")) {
            continue;
        }
        let name = field
            .file_name()
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .unwrap_or("Imported list")
            .to_string();
        let Ok(bytes) = field.bytes().await else {
            return refusal(
                StatusCode::BAD_REQUEST,
                "That file did not arrive whole. Files up to 64 MB work; try again.",
            );
        };
        found = Some((name, bytes.to_vec()));
    }
    let Some((name, bytes)) = found.filter(|(_, bytes)| !bytes.is_empty()) else {
        return choose();
    };
    let imports = state.imports.clone();
    let user = actor.user;
    match tokio::spawn(async move { imports.add_file(&user, &name, &bytes).await }).await {
        Ok(done) => result(&done),
        Err(_) => refusal(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Octo could not read that file.",
        ),
    }
}
