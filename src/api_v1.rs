use axum::{
    Json,
    body::Bytes,
    extract::{ConnectInfo, OriginalUri, Path, Query, State, rejection::QueryRejection},
    http::{HeaderMap, StatusCode},
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

fn unauthorized() -> Response {
    api_error(StatusCode::UNAUTHORIZED, "unauthorized")
}

/// Verifies an optional NIP-98 `Authorization` header against `state`'s configured
/// public origin and `uri`/`method`/`body`.
///
/// - `Ok(None)` — no `Authorization` header was present; caller should fall back to
///   whatever other authentication scheme it supports (or reject outright).
/// - `Ok(Some(pubkey))` — the header carried a valid, non-replayed NIP-98 event; the
///   64-char lowercase hex signer pubkey is returned as-is.
/// - `Err(response)` — the header was present but invalid (bad signature, wrong
///   URL/method/payload, stale, or replayed), or `public_base_url` is unset. Always a
///   401 `{"error":"unauthorized"}` response.
pub fn nip98_from_request(
    state: &AppState,
    headers: &HeaderMap,
    method: &str,
    uri: &OriginalUri,
    body: Option<&[u8]>,
) -> Result<Option<String>, Response> {
    let header = match headers.get(axum::http::header::AUTHORIZATION) {
        Some(value) => value,
        None => return Ok(None),
    };
    let header = header.to_str().map_err(|_| unauthorized())?;

    let base_url = state
        .config
        .public_base_url
        .as_deref()
        .ok_or_else(unauthorized)?;
    let origin =
        crate::nostr::announcement::normalized_origin(base_url).map_err(|_| unauthorized())?;
    let url = format!(
        "{origin}{}",
        uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("/")
    );

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let auth = crate::nostr::http_auth::verify_nip98(header, method, &url, body, now)
        .map_err(|_| unauthorized())?;

    if !state.nip98_guard.check_and_insert(&auth.event_id, now) {
        return Err(unauthorized());
    }

    Ok(Some(auth.pubkey))
}

pub async fn addresses_v1(
    State(state): State<AppState>,
    headers: HeaderMap,
    uri: OriginalUri,
) -> Response {
    match nip98_from_request(&state, &headers, "GET", &uri, None) {
        Ok(Some(pubkey)) => match state.repository.addresses_for_owner(&pubkey).await {
            Ok(addresses) => Json(json!({
                "addresses": addresses.iter().map(|a| json!({
                    "domain": a.domain,
                    "username": a.username,
                    "destination": a.destination,
                })).collect::<Vec<_>>()
            }))
            .into_response(),
            Err(_) => api_error(StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        },
        Ok(None) => api_error(StatusCode::UNAUTHORIZED, "unauthorized"),
        Err(response) => response,
    }
}

#[derive(Debug, Deserialize)]
pub struct QuoteQuery {
    pub domain: String,
    pub username: String,
}

pub async fn quote_v1(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    query: Result<Query<QuoteQuery>, QueryRejection>,
) -> Response {
    let Query(query) = match query {
        Ok(query) => query,
        Err(_) => return api_error(StatusCode::BAD_REQUEST, "invalid_input"),
    };
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

/// Resolves the owner for a registration request: a valid NIP-98 header always wins,
/// and if the body also carries an `owner_pubkey` the two must agree (else
/// `owner_mismatch`). With no NIP-98 header, the body's `owner_pubkey` (if any) is used
/// as-is.
fn resolve_owner(
    state: &AppState,
    headers: &HeaderMap,
    uri: &OriginalUri,
    raw_body: &[u8],
    body_owner_pubkey: &Option<String>,
) -> Result<Option<String>, Response> {
    match nip98_from_request(state, headers, "POST", uri, Some(raw_body))? {
        Some(pubkey) => match body_owner_pubkey {
            Some(existing) if existing != &pubkey => {
                Err(api_error(StatusCode::BAD_REQUEST, "owner_mismatch"))
            }
            _ => Ok(Some(pubkey)),
        },
        None => Ok(body_owner_pubkey.clone()),
    }
}

pub async fn register_v1(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    uri: OriginalUri,
    raw_body: Bytes,
) -> Response {
    let body: RegisterBody = match serde_json::from_slice(&raw_body) {
        Ok(body) => body,
        Err(_) => return api_error(StatusCode::BAD_REQUEST, "invalid_input"),
    };
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
    let owner = match resolve_owner(&state, &headers, &uri, &raw_body, &body.owner_pubkey) {
        Ok(owner) => owner,
        Err(response) => return response,
    };
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
        .register_lnaddr(
            &body.domain,
            &body.username,
            &body.destination,
            owner.as_deref(),
        )
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
    headers: HeaderMap,
    uri: OriginalUri,
    raw_body: Bytes,
) -> Response {
    let body: RegisterBody = match serde_json::from_slice(&raw_body) {
        Ok(body) => body,
        Err(_) => return api_error(StatusCode::BAD_REQUEST, "invalid_input"),
    };
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
    let owner = match resolve_owner(&state, &headers, &uri, &raw_body, &body.owner_pubkey) {
        Ok(owner) => owner,
        Err(response) => return response,
    };
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
        .start(
            &body.domain,
            &body.username,
            &body.destination,
            owner.as_deref(),
        )
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
    // `RegistrationManager::status()` returns `Err` both when the attempt is
    // missing *and* for transient failures (e.g. payment-verification outages)
    // on attempts that do exist. Distinguish those cases up front by checking
    // existence directly, so a temporary hiccup on a real attempt is reported
    // as a server error rather than incorrectly as "not_found".
    match state.repository.registration_attempt(&id).await {
        Ok(None) => return api_error(StatusCode::NOT_FOUND, "not_found"),
        Err(_) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        Ok(Some(_)) => {}
    }
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
        Err(_) => api_error(StatusCode::INTERNAL_SERVER_ERROR, "internal"),
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
    use axum::{Router, body::Body, extract::connect_info::MockConnectInfo, http::Request};
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

        let mut state = crate::test_app_state(repository, &domains, publisher, service)
            .await
            .unwrap();
        state.config = std::sync::Arc::new(crate::config::Config {
            public_base_url: Some("https://example.com".to_owned()),
            ..(*state.config).clone()
        });

        // keep the directory alive for the lifetime of the router by leaking it; the
        // temp files are only needed for the duration of the test process.
        std::mem::forget(directory);

        // Built via the same `crate::build_router` helper the real server uses (see
        // `normal_router` in `lib.rs`), so the public/private route split and the CORS
        // layer under test in `cors_allows_public_api_and_not_admin` are the exact
        // production wiring, not a hand-rolled approximation of it.
        let router: Router = crate::build_router(state);
        router.layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 1337))))
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
    async fn invalid_nip98_header_does_not_fall_back_to_body_token() {
        let app = test_router().await;

        let register_response = app
            .clone()
            .oneshot(
                Request::post("/api/v1/register")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "domain": "example.com",
                            "username": "carol",
                            "destination": "receiver@example.net",
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(register_response.status(), StatusCode::OK);
        let register_body: serde_json::Value = read_json(register_response).await;
        let token = register_body["management_token"]
            .as_str()
            .unwrap()
            .to_owned();

        let update_body = json!({
            "domain": "example.com",
            "username": "carol",
            "destination": "receiver2@example.net",
            "authentication_token": token,
        })
        .to_string();

        // A present-but-invalid NIP-98 header must never silently fall back to the
        // (valid) body token: the request must be rejected outright, not authorized
        // via the token.
        let response = app
            .clone()
            .oneshot(
                Request::put("/lnaddress/update")
                    .header("content-type", "application/json")
                    .header("authorization", "Nostr garbage")
                    .body(Body::from(update_body.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        // Confirms the token itself was valid and the 401 above came specifically
        // from the bad header, not from some other problem with the request.
        let response = app
            .oneshot(
                Request::put("/lnaddress/update")
                    .header("content-type", "application/json")
                    .body(Body::from(update_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn malformed_json_body_is_reported_as_invalid_input() {
        let app = test_router().await;
        let response = app
            .oneshot(
                Request::post("/api/v1/register")
                    .header("content-type", "application/json")
                    .body(Body::from("not json"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body: serde_json::Value = read_json(response).await;
        assert_eq!(body["error"], "invalid_input");
    }

    #[tokio::test]
    async fn missing_query_param_is_reported_as_invalid_input() {
        let app = test_router().await;
        let response = app
            .oneshot(
                Request::get("/api/v1/register/quote?domain=example.com")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body: serde_json::Value = read_json(response).await;
        assert_eq!(body["error"], "invalid_input");
    }

    #[tokio::test]
    async fn invalid_owner_pubkey_is_rejected() {
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
                            "owner_pubkey": "XYZ",
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body: serde_json::Value = read_json(response).await;
        assert_eq!(body["error"], "invalid_input");
    }

    #[tokio::test]
    async fn cors_allows_public_api_and_not_admin() {
        let app = test_router().await;
        let response = app
            .clone()
            .oneshot(
                Request::get("/api/v1/register/quote?domain=example.com&username=alice")
                    .header("origin", "https://market.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response
                .headers()
                .get("access-control-allow-origin")
                .unwrap(),
            "*"
        );
        let preflight = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("OPTIONS")
                    .uri("/api/v1/register")
                    .header("origin", "https://market.example")
                    .header("access-control-request-method", "POST")
                    .header(
                        "access-control-request-headers",
                        "content-type, authorization",
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(preflight.status(), StatusCode::OK);
        let admin = app
            .oneshot(
                Request::get("/admin/login")
                    .header("origin", "https://market.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(admin.headers().get("access-control-allow-origin").is_none());
    }

    fn now_secs() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    #[tokio::test]
    async fn addresses_requires_and_honors_nip98() {
        use crate::nostr::http_auth::auth_header;
        use nostr_sdk::prelude::Keys;

        let app = test_router().await;
        let keys = Keys::generate();
        let other_keys = Keys::generate();

        let register_body = json!({
            "domain": "example.com",
            "username": "alice",
            "destination": "receiver@example.net",
        })
        .to_string();
        let register_header = auth_header(
            &keys,
            "https://example.com/api/v1/register",
            "POST",
            Some(register_body.as_bytes()),
            now_secs(),
        );
        let response = app
            .clone()
            .oneshot(
                Request::post("/api/v1/register")
                    .header("content-type", "application/json")
                    .header("authorization", register_header)
                    .body(Body::from(register_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: serde_json::Value = read_json(response).await;
        assert_eq!(body["address"], "alice@example.com");

        let get_header = auth_header(
            &keys,
            "https://example.com/api/v1/addresses",
            "GET",
            None,
            now_secs(),
        );
        let response = app
            .clone()
            .oneshot(
                Request::get("/api/v1/addresses")
                    .header("authorization", get_header)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: serde_json::Value = read_json(response).await;
        let addresses = body["addresses"].as_array().unwrap();
        assert_eq!(addresses.len(), 1);
        assert_eq!(addresses[0]["domain"], "example.com");
        assert_eq!(addresses[0]["username"], "alice");
        assert_eq!(addresses[0]["destination"], "receiver@example.net");

        let response = app
            .clone()
            .oneshot(
                Request::get("/api/v1/addresses")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let other_header = auth_header(
            &other_keys,
            "https://example.com/api/v1/addresses",
            "GET",
            None,
            now_secs(),
        );
        let response = app
            .oneshot(
                Request::get("/api/v1/addresses")
                    .header("authorization", other_header)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: serde_json::Value = read_json(response).await;
        assert_eq!(body["addresses"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn owner_mismatch_is_rejected() {
        use crate::nostr::http_auth::auth_header;
        use nostr_sdk::prelude::Keys;

        let app = test_router().await;
        let keys_a = Keys::generate();
        let keys_b = Keys::generate();

        let register_body = json!({
            "domain": "example.com",
            "username": "bob",
            "destination": "receiver@example.net",
            "owner_pubkey": keys_b.public_key().to_string(),
        })
        .to_string();
        let register_header = auth_header(
            &keys_a,
            "https://example.com/api/v1/register",
            "POST",
            Some(register_body.as_bytes()),
            now_secs(),
        );
        let response = app
            .oneshot(
                Request::post("/api/v1/register")
                    .header("content-type", "application/json")
                    .header("authorization", register_header)
                    .body(Body::from(register_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body: serde_json::Value = read_json(response).await;
        assert_eq!(body["error"], "owner_mismatch");
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
