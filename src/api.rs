use anyhow::Result;
use axum::{
    Json,
    extract::{Host, Path, State},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::AppState;
use crate::service::RegisterResponse;

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

pub async fn get_lnaddr_manifest_handler(
    State(state): State<AppState>,
    Host(domain): Host,
    Path(username): Path<String>,
) -> Result<Json<lnurl::pay::PayResponse>, axum::http::StatusCode> {
    state
        .service
        .get_lnaddr_manifest(&domain, &username)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(axum::http::StatusCode::NOT_FOUND)
        .map(Json)
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
    Json(payload): Json<RegisterRequest>,
) -> Result<Json<RegisterResponse>, axum::http::StatusCode> {
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
