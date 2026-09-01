use std::{sync::Arc, time::Duration};

use axum::{
    Form, Router,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use maud::{DOCTYPE, html};
use serde::Deserialize;
use subtle::ConstantTimeEq;
use tokio::sync::Notify;

use crate::{
    admin::{AdminAuth, SESSION_COOKIE},
    config::Config,
    crypto::RootSecret,
    initialize_empty,
    nostr::{publisher::NostrPublisher, restore},
    repository::sqlite::SqlitePaymentAddressRepository,
    restore_database,
};

#[derive(Clone)]
pub struct BootstrapState {
    pub config: Arc<Config>,
    pub admin_auth: Arc<AdminAuth>,
    pub restart: Arc<Notify>,
}

#[derive(Deserialize)]
struct LoginForm {
    password: String,
}

#[derive(Deserialize)]
struct SetupForm {
    csrf_token: String,
}

#[derive(Deserialize)]
struct RecoverForm {
    csrf_token: String,
    root_seed: String,
}

pub fn router(state: BootstrapState) -> Router {
    Router::new()
        .route("/health/live", get(|| async { StatusCode::OK }))
        .route("/admin", get(setup_page))
        .route("/admin/login", get(login_page).post(login_submit))
        .route("/admin/setup/fresh", post(fresh_submit))
        .route("/admin/setup/recover", post(recover_submit))
        .with_state(state)
        .fallback(|| async { StatusCode::SERVICE_UNAVAILABLE })
}

async fn login_page() -> Html<String> {
    Html(login_markup(None))
}

async fn login_submit(
    State(state): State<BootstrapState>,
    Form(form): Form<LoginForm>,
) -> Response {
    match state.admin_auth.login(&form.password).await {
        Ok(Some(session)) => {
            let cookie = format!(
                "{SESSION_COOKIE}={}; Path=/admin; Max-Age=43200; Secure; HttpOnly; SameSite=Strict",
                session.token
            );
            ([(header::SET_COOKIE, cookie)], Redirect::to("/admin")).into_response()
        }
        Ok(None) => (
            StatusCode::UNAUTHORIZED,
            Html(login_markup(Some("Invalid password or too many attempts"))),
        )
            .into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn setup_page(State(state): State<BootstrapState>, headers: HeaderMap) -> Response {
    let session = match state.admin_auth.authenticate(&headers).await {
        Ok(Some(session)) => session,
        Ok(None) => return Redirect::to("/admin/login").into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let seed_exists = state.config.root_secret_file.exists();
    Html(
        html! {
            (DOCTYPE)
            html lang="en" {
                head {
                    meta charset="UTF-8";
                    meta name="viewport" content="width=device-width, initial-scale=1.0";
                    meta name="robots" content="noindex,nofollow";
                    title { "Set up lnaddrd" }
                }
                body { main {
                    h1 { "Set up lnaddrd" }
                    p { "The administrator password unlocks setup. Choose exactly one option." }
                    section {
                        h2 { "Recover an existing service" }
                        p { "Enter the 64-character root seed. lnaddrd validates the remote backup before installing it locally." }
                        form method="post" action="/admin/setup/recover" {
                            input type="hidden" name="csrf_token" value=(session.csrf_token);
                            label for="root-seed" { "Root seed" }
                            input id="root-seed" name="root_seed" type="password" minlength="64" maxlength="64" required autocomplete="off";
                            button type="submit" { "Validate and recover" }
                        }
                    }
                    section {
                        h2 { "Create a fresh service" }
                        @if seed_exists {
                            p { "A root seed is already installed. This retries fresh initialization with that seed and cannot recover old addresses." }
                        } @else {
                            p { "This generates a new root seed and publishes an empty initial configuration. It cannot recover old addresses." }
                        }
                        form method="post" action="/admin/setup/fresh" {
                            input type="hidden" name="csrf_token" value=(session.csrf_token);
                            button type="submit" {
                                @if seed_exists { "Retry fresh initialization" } @else { "Generate fresh seed" }
                            }
                        }
                    }
                } }
            }
        }
        .into_string(),
    )
    .into_response()
}

async fn fresh_submit(
    State(state): State<BootstrapState>,
    headers: HeaderMap,
    Form(form): Form<SetupForm>,
) -> Response {
    let session = match authenticated(&state, &headers).await {
        Ok(session) => session,
        Err(response) => return response,
    };
    if !csrf_matches(&session.csrf_token, &form.csrf_token) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let seed = if state.config.root_secret_file.exists() {
        RootSecret::load(&state.config.root_secret_file)
    } else {
        RootSecret::create(&state.config.root_secret_file)
    };
    if let Err(error) = seed {
        return setup_error(&error.to_string());
    }
    match initialize_empty(&state.config).await {
        Ok(()) => finish_setup(&state, "Fresh service initialized."),
        Err(error) => setup_error(&error.to_string()),
    }
}

async fn recover_submit(
    State(state): State<BootstrapState>,
    headers: HeaderMap,
    Form(form): Form<RecoverForm>,
) -> Response {
    let session = match authenticated(&state, &headers).await {
        Ok(session) => session,
        Err(response) => return response,
    };
    if !csrf_matches(&session.csrf_token, &form.csrf_token) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let candidate = match RootSecret::from_hex(&form.root_seed) {
        Ok(secret) => secret,
        Err(error) => return setup_error(&error.to_string()),
    };
    let validation = async {
        let temporary = tempfile::tempdir()?;
        let repository = SqlitePaymentAddressRepository::new(
            temporary
                .path()
                .join("restore.sqlite3")
                .to_string_lossy()
                .as_ref(),
        )?;
        let network = NostrPublisher::connect(&state.config.nostr_relays).await?;
        restore::restore(
            &repository,
            &network,
            Arc::new(candidate.derive()?),
            &state.config.domains,
            true,
        )
        .await
    }
    .await;
    if let Err(error) = validation {
        return setup_error(&format!("Recovery validation failed: {error}"));
    }
    if let Err(error) = RootSecret::install(&state.config.root_secret_file, &form.root_seed) {
        return setup_error(&error.to_string());
    }
    match restore_database(&state.config, false).await {
        Ok(()) => finish_setup(&state, "Service recovered from Nostr."),
        Err(error) => setup_error(&error.to_string()),
    }
}

async fn authenticated(
    state: &BootstrapState,
    headers: &HeaderMap,
) -> Result<crate::admin::AdminSession, Response> {
    match state.admin_auth.authenticate(headers).await {
        Ok(Some(session)) => Ok(session),
        _ => Err(StatusCode::UNAUTHORIZED.into_response()),
    }
}

fn csrf_matches(expected: &str, supplied: &str) -> bool {
    expected.as_bytes().ct_eq(supplied.as_bytes()).into()
}

fn finish_setup(state: &BootstrapState, message: &str) -> Response {
    let restart = state.restart.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        restart.notify_one();
    });
    Html(html! { h1 { (message) } p { "Starting the service…" } }.into_string()).into_response()
}

fn setup_error(message: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Html(
            html! { h1 { "Setup failed" } p role="alert" { (message) } a href="/admin" { "Back" } }
                .into_string(),
        ),
    )
        .into_response()
}

fn login_markup(error: Option<&str>) -> String {
    html! {
        (DOCTYPE)
        html lang="en" { body { main {
            h1 { "lnaddrd setup login" }
            @if let Some(error) = error { p role="alert" { (error) } }
            form method="post" action="/admin/login" {
                label for="password" { "Administrator password" }
                input id="password" type="password" name="password" required autofocus;
                button type="submit" { "Log in" }
            }
        } } }
    }
    .into_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use std::net::SocketAddr;
    use tower::ServiceExt;

    #[tokio::test]
    async fn bootstrap_router_exposes_only_setup_and_liveness() {
        let directory = tempfile::tempdir().unwrap();
        let repository = SqlitePaymentAddressRepository::new(
            directory
                .path()
                .join("db.sqlite3")
                .to_string_lossy()
                .as_ref(),
        )
        .unwrap();
        let password_path = directory.path().join("state/admin-password");
        let auth = Arc::new(AdminAuth::load_or_create(&password_path, repository).unwrap());
        let config = Config {
            operation: None,
            domains: vec!["example.com".to_owned()],
            bind: "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
            database_path: directory.path().join("db.sqlite3").to_string_lossy().into(),
            root_secret_file: directory.path().join("state/root-secret"),
            admin_password_file: password_path,
            nostr_relays: vec![],
            public_base_url: None,
            service_name: "test".to_owned(),
            warning: None,
        };
        let app = router(BootstrapState {
            config: Arc::new(config),
            admin_auth: auth,
            restart: Arc::new(Notify::new()),
        });

        let live = app
            .clone()
            .oneshot(Request::get("/health/live").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(live.status(), StatusCode::OK);
        let public = app
            .clone()
            .oneshot(
                Request::get("/.well-known/lnurlp/alice")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(public.status(), StatusCode::SERVICE_UNAVAILABLE);
        let admin = app
            .oneshot(Request::get("/admin").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(admin.status(), StatusCode::SEE_OTHER);
    }
}
