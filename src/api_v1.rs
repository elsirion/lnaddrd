use axum::{
    Json,
    extract::{ConnectInfo, Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::json;
use std::net::SocketAddr;

use crate::{
    AppState,
    registration::{QuoteRejection, RegistrationStatus},
};

fn api_error(status: StatusCode, code: &str) -> Response {
    (status, Json(json!({ "error": code }))).into_response()
}

fn rejection_response(rejection: QuoteRejection) -> Response {
    let status = match rejection {
        QuoteRejection::Taken | QuoteRejection::Reserved => StatusCode::CONFLICT,
        _ => StatusCode::BAD_REQUEST,
    };
    api_error(status, rejection.code())
}

#[derive(Debug, Deserialize)]
pub struct QuoteQuery {
    pub domain: String,
    pub username: String,
}

pub async fn quote_v1(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Query(query): Query<QuoteQuery>,
) -> Response {
    if !state
        .registration_manager
        .allow_request(peer.ip(), "quote", 30)
        .await
    {
        return api_error(StatusCode::TOO_MANY_REQUESTS, "rate_limited");
    }
    match state
        .registration_manager
        .quote_checked(&query.domain, &query.username)
        .await
    {
        Ok(Ok(price_msat)) => Json(json!({ "price_msat": price_msat })).into_response(),
        Ok(Err(rejection)) => rejection_response(rejection),
        Err(_) => api_error(StatusCode::INTERNAL_SERVER_ERROR, "internal"),
    }
}

#[derive(Debug, Deserialize)]
pub struct RegisterBody {
    pub domain: String,
    pub username: String,
    pub destination: String,
    #[serde(default)]
    pub owner_pubkey: Option<String>,
}

fn validate_owner_pubkey(value: &Option<String>) -> Result<(), Response> {
    if let Some(pubkey) = value {
        let valid = pubkey.len() == 64
            && pubkey
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase());
        if !valid {
            return Err(api_error(StatusCode::BAD_REQUEST, "invalid_input"));
        }
    }
    Ok(())
}

pub async fn register_v1(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(body): Json<RegisterBody>,
) -> Response {
    if !state
        .registration_manager
        .allow_request(peer.ip(), "start", 10)
        .await
    {
        return api_error(StatusCode::TOO_MANY_REQUESTS, "rate_limited");
    }
    if let Err(response) = validate_owner_pubkey(&body.owner_pubkey) {
        return response;
    }
    match state
        .registration_manager
        .quote_checked(&body.domain, &body.username)
        .await
    {
        Ok(Ok(0)) => {}
        Ok(Ok(_)) => return api_error(StatusCode::BAD_REQUEST, "payment_required"),
        Ok(Err(rejection)) => return rejection_response(rejection),
        Err(_) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, "internal"),
    }
    match state
        .service
        .register_lnaddr(&body.domain, &body.username, &body.destination)
        .await
    {
        Ok(response) => Json(json!({
            "address": response.lnaddr,
            "management_token": response.authentication_token,
            "active": response.active,
        }))
        .into_response(),
        Err(_) => api_error(StatusCode::BAD_REQUEST, "invalid_input"),
    }
}

pub async fn register_start_v1(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(body): Json<RegisterBody>,
) -> Response {
    if !state
        .registration_manager
        .allow_request(peer.ip(), "start", 10)
        .await
    {
        return api_error(StatusCode::TOO_MANY_REQUESTS, "rate_limited");
    }
    if let Err(response) = validate_owner_pubkey(&body.owner_pubkey) {
        return response;
    }
    match state
        .registration_manager
        .quote_checked(&body.domain, &body.username)
        .await
    {
        Ok(Ok(0)) => return api_error(StatusCode::BAD_REQUEST, "free_registration"),
        Ok(Ok(_)) => {}
        Ok(Err(rejection)) => return rejection_response(rejection),
        Err(_) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, "internal"),
    }
    match state
        .registration_manager
        .start(&body.domain, &body.username, &body.destination)
        .await
    {
        Ok(started) => Json(json!({
            "id": started.id,
            "bolt11": started.invoice,
            "amount_msat": started.amount_msat,
            "expires_at": started.expires_at,
        }))
        .into_response(),
        Err(_) => api_error(StatusCode::BAD_REQUEST, "invalid_input"),
    }
}

pub async fn register_status_v1(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.registration_manager.status(&id).await {
        Ok(RegistrationStatus::Pending) => {
            Json(json!({ "state": "pending_payment" })).into_response()
        }
        Ok(RegistrationStatus::Publishing) => {
            Json(json!({ "state": "publishing" })).into_response()
        }
        Ok(RegistrationStatus::Expired) => Json(json!({ "state": "expired" })).into_response(),
        Ok(RegistrationStatus::Complete {
            address,
            management_token,
        }) => Json(json!({
            "state": "complete",
            "address": address,
            "management_token": management_token,
        }))
        .into_response(),
        Err(_) => api_error(StatusCode::NOT_FOUND, "not_found"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nostr::publisher::{EventPublisher, Publication};
    use crate::payment::DestinationValidator;
    use crate::repository::DestinationPaymentAddress;
    use crate::repository::sqlite::SqlitePaymentAddressRepository;
    use crate::service::direct::DirectLnaddrService;
    use anyhow::Result as AnyResult;
    use async_trait::async_trait;
    use axum::{
        Router,
        body::Body,
        extract::connect_info::MockConnectInfo,
        http::Request,
        routing::{get, post},
    };
    use nostr_sdk::prelude::Event;
    use std::net::SocketAddr;
    use tower::util::ServiceExt;

    struct FixturePublisher;

    #[async_trait]
    impl EventPublisher for FixturePublisher {
        async fn publish(&self, _event: &Event) -> AnyResult<Publication> {
            Ok(Publication {
                accepted_by: Vec::new(),
                failed: Vec::new(),
            })
        }
    }

    struct NoopDestinationValidator;

    #[async_trait]
    impl DestinationValidator for NoopDestinationValidator {
        async fn validate(&self, _destination: &DestinationPaymentAddress) -> AnyResult<()> {
            Ok(())
        }
    }

    async fn test_router() -> Router {
        let directory = tempfile::tempdir().unwrap();
        let repository = SqlitePaymentAddressRepository::new(
            directory.path().join("db.sqlite3").to_str().unwrap(),
        )
        .unwrap();
        repository
            .set_metadata("instance_id", &"01".repeat(32))
            .await
            .unwrap();
        repository
            .set_metadata("configuration_revision", "1")
            .await
            .unwrap();

        let publisher: crate::nostr::publisher::Publisher = std::sync::Arc::new(FixturePublisher);
        let domains = vec!["example.com".to_owned()];
        let mut direct_service = DirectLnaddrService::new(
            repository.clone(),
            domains.clone(),
            std::sync::Arc::new(
                crate::crypto::RootSecret::from_bytes([0x42; 32])
                    .derive()
                    .unwrap(),
            ),
            publisher.clone(),
        )
        .unwrap();
        direct_service.set_destination_validator(std::sync::Arc::new(NoopDestinationValidator));
        let service = direct_service.into_dyn();

        let state = crate::test_app_state(repository, &domains, publisher, service)
            .await
            .unwrap();

        // keep the directory alive for the lifetime of the router by leaking it; the
        // temp files are only needed for the duration of the test process.
        std::mem::forget(directory);

        Router::new()
            .route("/api/v1/register/quote", get(quote_v1))
            .route("/api/v1/register", post(register_v1))
            .route("/api/v1/register/start", post(register_start_v1))
            .route("/api/v1/register/:id", get(register_status_v1))
            .layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 1337))))
            .with_state(state)
    }

    async fn read_json(response: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn quote_returns_price_and_structured_errors() {
        let app = test_router().await;
        let response = app
            .clone()
            .oneshot(
                Request::get("/api/v1/register/quote?domain=example.com&username=alice")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: serde_json::Value = read_json(response).await;
        assert_eq!(body["price_msat"], 0);

        let response = app
            .oneshot(
                Request::get("/api/v1/register/quote?domain=nope.com&username=alice")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body: serde_json::Value = read_json(response).await;
        assert_eq!(body["error"], "unsupported_domain");
    }

    #[tokio::test]
    async fn free_registration_returns_token() {
        let app = test_router().await;
        let response = app
            .oneshot(
                Request::post("/api/v1/register")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "domain": "example.com",
                            "username": "alice",
                            "destination": "receiver@example.net",
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: serde_json::Value = read_json(response).await;
        assert_eq!(body["address"], "alice@example.com");
        assert!(body["management_token"].as_str().is_some());
    }

    #[tokio::test]
    async fn unknown_attempt_is_not_found() {
        let app = test_router().await;
        let response = app
            .oneshot(
                Request::get("/api/v1/register/does-not-exist")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body: serde_json::Value = read_json(response).await;
        assert_eq!(body["error"], "not_found");
    }
}
