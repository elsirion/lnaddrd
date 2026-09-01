use anyhow::Result;
use axum::{
    Json,
    extract::{ConnectInfo, Host, Path, Query, State},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::net::SocketAddr;

use crate::AppState;
use crate::domain::{Domain, Username};
use crate::nostr::announcement::well_known;
use crate::service::RegisterResponse;

const HTMX_JS: &str = include_str!("../assets/htmx-4.0.0.min.js");

pub async fn htmx_asset_handler() -> impl axum::response::IntoResponse {
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/javascript; charset=utf-8",
        )],
        HTMX_JS,
    )
}

pub async fn well_known_announcement_handler(
    State(state): State<AppState>,
) -> Result<Json<crate::nostr::announcement::WellKnownAnnouncement>, axum::http::StatusCode> {
    well_known(&state.config, &state.keys)
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?
        .map(Json)
        .ok_or(axum::http::StatusCode::NOT_FOUND)
}

pub async fn liveness_handler() -> axum::http::StatusCode {
    axum::http::StatusCode::NO_CONTENT
}

pub async fn readiness_handler(State(state): State<AppState>) -> axum::http::StatusCode {
    match state.repository.metadata("initialized").await {
        Ok(Some(value)) if value == "true" => axum::http::StatusCode::NO_CONTENT,
        _ => axum::http::StatusCode::SERVICE_UNAVAILABLE,
    }
}

pub async fn list_domains_handler(
    State(state): State<AppState>,
) -> Result<Json<Vec<String>>, axum::http::StatusCode> {
    state
        .service
        .list_domains()
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)
        .map(Json)
}

#[derive(Debug, Deserialize)]
pub struct ManifestQuery {
    pub min_sendable: Option<u64>,
    pub max_sendable: Option<u64>,
}

pub async fn get_lnaddr_manifest_handler(
    State(state): State<AppState>,
    Host(domain): Host,
    Path(username): Path<String>,
    Query(query): Query<ManifestQuery>,
) -> Result<Json<lnurl::pay::PayResponse>, axum::http::StatusCode> {
    let domain = host_without_port(&domain)?;
    let mut response = state
        .service
        .get_lnaddr_manifest(&domain, &username)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(axum::http::StatusCode::NOT_FOUND)?;

    let min = query.min_sendable.unwrap_or(response.min_sendable);
    let max = query.max_sendable.unwrap_or(response.max_sendable);

    if min < response.min_sendable || max > response.max_sendable || min > max {
        return Err(axum::http::StatusCode::BAD_REQUEST);
    }

    response.min_sendable = min;
    response.max_sendable = max;

    Ok(Json(response))
}

pub async fn get_lnaddr_handler(
    State(state): State<AppState>,
    Path((domain, username)): Path<(String, String)>,
) -> Result<Json<Value>, axum::http::StatusCode> {
    state
        .service
        .get_destination(&domain, &username)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(axum::http::StatusCode::NOT_FOUND)
        .map(|d| Json(json!({ "url": d.url() })))
}

pub async fn register_lnaddr_handler(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(payload): Json<RegisterRequest>,
) -> Result<Json<RegisterResponse>, axum::http::StatusCode> {
    if !state
        .registration_manager
        .allow_request(peer.ip(), "start", 10)
        .await
    {
        return Err(axum::http::StatusCode::TOO_MANY_REQUESTS);
    }
    state
        .service
        .register_lnaddr(&payload.domain, &payload.username, &payload.lnurl)
        .await
        .map_err(|_| axum::http::StatusCode::BAD_REQUEST)
        .map(Json)
}

pub async fn remove_lnaddr_handler(
    State(state): State<AppState>,
    Json(payload): Json<RemoveRequest>,
) -> Result<axum::http::StatusCode, axum::http::StatusCode> {
    state
        .service
        .remove_lnaddr(
            &payload.domain,
            &payload.username,
            &payload.authentication_token,
        )
        .await
        .map_err(|_| axum::http::StatusCode::UNAUTHORIZED)?;

    Ok(axum::http::StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
pub struct UpdateRequest {
    pub domain: String,
    pub username: String,
    pub destination: String,
    pub authentication_token: String,
}

pub async fn update_lnaddr_handler(
    State(state): State<AppState>,
    Json(payload): Json<UpdateRequest>,
) -> Result<Json<Value>, axum::http::StatusCode> {
    state
        .service
        .update_lnaddr(
            &payload.domain,
            &payload.username,
            &payload.destination,
            &payload.authentication_token,
        )
        .await
        .map(|active| Json(json!({ "active": active })))
        .map_err(|_| axum::http::StatusCode::BAD_REQUEST)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterRequest {
    pub domain: String,
    pub username: String,
    pub lnurl: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RemoveRequest {
    pub domain: String,
    pub username: String,
    pub authentication_token: String,
}

#[derive(Debug, Deserialize)]
pub struct GenerateLnurlQuery {
    pub min_sendable: u64,
    pub max_sendable: u64,
}

pub async fn generate_lnurl_handler(
    Host(domain): Host,
    Path(username): Path<String>,
    Query(query): Query<GenerateLnurlQuery>,
) -> Result<Json<Value>, axum::http::StatusCode> {
    let domain = host_without_port(&domain)?
        .parse::<Domain>()
        .map_err(|_| axum::http::StatusCode::BAD_REQUEST)?;
    let username = username
        .parse::<Username>()
        .map_err(|_| axum::http::StatusCode::BAD_REQUEST)?;
    if query.min_sendable > query.max_sendable {
        return Err(axum::http::StatusCode::BAD_REQUEST);
    }

    let url = format!(
        "https://{domain}/.well-known/lnurlp/{username}?min_sendable={}&max_sendable={}",
        query.min_sendable, query.max_sendable
    );
    let lnurl = lnurl::lnurl::LnUrl::from_url(url);

    Ok(Json(json!({ "lnurl": lnurl.encode().to_uppercase() })))
}

fn host_without_port(host: &str) -> Result<String, axum::http::StatusCode> {
    host.parse::<axum::http::uri::Authority>()
        .map(|authority| authority.host().to_owned())
        .map_err(|_| axum::http::StatusCode::BAD_REQUEST)
}
