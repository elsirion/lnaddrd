use std::{
    collections::VecDeque,
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, ensure};
use argon2::{
    Argon2, PasswordHash, PasswordHasher, PasswordVerifier,
    password_hash::{SaltString, rand_core::OsRng as PasswordOsRng},
};
use axum::{
    Form,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{Html, IntoResponse, Redirect, Response},
};
use maud::{DOCTYPE, html};
use rand::{RngCore, rngs::OsRng};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::sync::Mutex;
use tracing::info;

use crate::domain::{Destination, Domain, Username};
use crate::nostr::announcement::build_event;
use crate::nostr::codec::{AddressRecord, BackupCodec, ServiceConfigurationRecord, UpdatedBy};
use crate::nostr::restore;
use crate::payment::{PaymentClient, parse_policy};
use crate::{AppState, crypto::RootSecret, repository::sqlite::SqlitePaymentAddressRepository};

pub(crate) const SESSION_COOKIE: &str = "lnaddrd_admin";
const SESSION_DURATION: Duration = Duration::from_secs(12 * 60 * 60);

pub struct AdminAuth {
    password_hash: String,
    password_fingerprint: String,
    repository: SqlitePaymentAddressRepository,
    failed_logins: Mutex<VecDeque<std::time::Instant>>,
}

#[derive(Debug, Clone)]
pub struct AdminSession {
    pub token: String,
    pub csrf_token: String,
    pub expires_at: i64,
}

impl AdminAuth {
    pub fn load_or_create(path: &Path, repository: SqlitePaymentAddressRepository) -> Result<Self> {
        let password = load_or_create_password(path)?;
        let fingerprint: [u8; 32] = Sha256::digest(password.as_bytes()).into();
        let salt = SaltString::generate(&mut PasswordOsRng);
        let password_hash = Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map_err(|error| anyhow::anyhow!("Could not hash admin password: {error}"))?
            .to_string();
        Ok(Self {
            password_fingerprint: hex::encode(fingerprint),
            password_hash,
            repository,
            failed_logins: Mutex::new(VecDeque::new()),
        })
    }

    pub async fn login(&self, candidate: &str) -> Result<Option<AdminSession>> {
        let now = std::time::Instant::now();
        let mut failed = self.failed_logins.lock().await;
        while failed
            .front()
            .is_some_and(|time| now.duration_since(*time) > Duration::from_secs(60))
        {
            failed.pop_front();
        }
        if failed.len() >= 5 {
            return Ok(None);
        }

        let valid = PasswordHash::new(&self.password_hash)
            .ok()
            .is_some_and(|hash| {
                Argon2::default()
                    .verify_password(candidate.as_bytes(), &hash)
                    .is_ok()
            });
        if !valid {
            failed.push_back(now);
            return Ok(None);
        }
        failed.clear();
        drop(failed);

        let token = random_hex();
        let csrf_token = random_hex();
        let expires_at = unix_now()?.saturating_add(i64::try_from(SESSION_DURATION.as_secs())?);
        self.repository
            .create_admin_session(
                &hash_token(&token),
                &self.password_fingerprint,
                &csrf_token,
                expires_at,
            )
            .await?;
        Ok(Some(AdminSession {
            token,
            csrf_token,
            expires_at,
        }))
    }

    pub fn verify_password(&self, candidate: &str) -> bool {
        PasswordHash::new(&self.password_hash)
            .ok()
            .is_some_and(|hash| {
                Argon2::default()
                    .verify_password(candidate.as_bytes(), &hash)
                    .is_ok()
            })
    }

    pub async fn authenticate(&self, headers: &HeaderMap) -> Result<Option<AdminSession>> {
        let Some(token) = cookie_value(headers, SESSION_COOKIE) else {
            return Ok(None);
        };
        let Some(record) = self
            .repository
            .admin_session(&hash_token(&token), &self.password_fingerprint)
            .await?
        else {
            return Ok(None);
        };
        Ok(Some(AdminSession {
            token,
            csrf_token: record.csrf_token,
            expires_at: record.expires_at,
        }))
    }

    pub async fn logout(&self, session: &AdminSession) -> Result<()> {
        self.repository
            .delete_admin_session(&hash_token(&session.token))
            .await
    }
}

#[derive(Deserialize)]
pub struct LoginForm {
    password: String,
}

#[derive(Deserialize)]
pub struct LogoutForm {
    csrf_token: String,
}

#[derive(Deserialize)]
pub struct ReservedNameForm {
    csrf_token: String,
    domain: String,
    username: String,
    reserved: bool,
}

#[derive(Deserialize)]
pub struct PaymentPolicyForm {
    csrf_token: String,
    domain: String,
    destination: String,
    tiers: String,
    #[serde(default)]
    enabled: bool,
}

#[derive(Deserialize)]
pub struct AddressActionForm {
    csrf_token: String,
    domain: String,
    username: String,
}

#[derive(Deserialize)]
pub struct SeedExportForm {
    csrf_token: String,
    password: String,
}

pub async fn login_page() -> Html<String> {
    Html(login_markup(None))
}

pub async fn login_submit(State(state): State<AppState>, Form(form): Form<LoginForm>) -> Response {
    match state.admin_auth.login(&form.password).await {
        Ok(Some(session)) => {
            let cookie = format!(
                "{SESSION_COOKIE}={}; Path=/admin; Max-Age={}; Secure; HttpOnly; SameSite=Strict",
                session.token,
                SESSION_DURATION.as_secs()
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

pub async fn dashboard(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let session = match state.admin_auth.authenticate(&headers).await {
        Ok(Some(session)) => session,
        Ok(None) => return Redirect::to("/admin/login").into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let pending = state
        .repository
        .pending_event_count()
        .await
        .unwrap_or_default();
    let configuration = match state.configuration_manager.current().await {
        Ok(configuration) => configuration,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let addresses = state.repository.admin_addresses().await.unwrap_or_default();
    let relays = state.repository.relay_health().await.unwrap_or_default();
    let markup = html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="UTF-8";
                meta name="viewport" content="width=device-width, initial-scale=1.0";
                title { "lnaddrd administration" }
                script src="/assets/htmx-4.0.0.min.js" {}
            }
            body {
                main {
                    h1 { "lnaddrd administration" }
                    p { "Service public key: " code { (state.keys.service_public_key()) } }
                    p { "Pending Nostr events: " (pending) }
                    h2 { "Relay health" }
                    table {
                        tr { th { "Relay" } th { "Last success" } th { "Last error" } }
                        @for relay in relays {
                            tr {
                                td { code { (relay.relay_url) } }
                                td { (relay.last_success_at.map_or_else(|| "never".to_owned(), |value| value.to_string())) }
                                td { (relay.last_error.unwrap_or_default()) }
                            }
                        }
                    }
                    h2 { "Addresses" }
                    table {
                        tr { th { "Address" } th { "Destination" } th { "State" } th { "Revision" } th { "Actions" } }
                        @for address in addresses {
                            tr {
                                td { (address.username) "@" (address.domain) }
                                td { code { (address.destination) } }
                                td { (address.state) }
                                td { (address.revision) }
                                td {
                                    @if address.backup_event_id.is_some() {
                                        form method="post" action="/admin/address/retry" style="display:inline"
                                            hx-post="/admin/address/retry" hx-target="#admin-action-result" {
                                            input type="hidden" name="csrf_token" value=(session.csrf_token);
                                            input type="hidden" name="domain" value=(address.domain);
                                            input type="hidden" name="username" value=(address.username);
                                            button type="submit" { "Retry backup" }
                                        }
                                    }
                                    form method="post" action="/admin/address/delete" style="display:inline"
                                        hx-post="/admin/address/delete" hx-target="#admin-action-result"
                                        hx-confirm=(format!("Delete {}@{}?", address.username, address.domain)) {
                                        input type="hidden" name="csrf_token" value=(session.csrf_token);
                                        input type="hidden" name="domain" value=(address.domain);
                                        input type="hidden" name="username" value=(address.username);
                                        button type="submit" { "Delete" }
                                    }
                                }
                            }
                        }
                    }
                    div id="admin-action-result" {}
                    form method="post" action="/admin/restore-dry-run"
                        hx-post="/admin/restore-dry-run" hx-target="#admin-action-result" {
                        input type="hidden" name="csrf_token" value=(session.csrf_token);
                        button type="submit" { "Compare with Nostr backup" }
                    }
                    h2 { "Root seed backup" }
                    p { "The root seed is the only irreplaceable service state. Export it only to secure offline storage." }
                    form method="post" action="/admin/seed/export" {
                        input type="hidden" name="csrf_token" value=(session.csrf_token);
                        label for="seed-password" { "Confirm admin password" }
                        input id="seed-password" type="password" name="password" required;
                        button type="submit" { "Export root seed" }
                    }
                    section id="configuration" {
                        (configuration_markup(&configuration, &session.csrf_token, None))
                    }
                    form method="post" action="/admin/logout" {
                        input type="hidden" name="csrf_token" value=(session.csrf_token);
                        button type="submit" { "Log out" }
                    }
                }
            }
        }
    };
    Html(markup.into_string()).into_response()
}

pub async fn seed_export_submit(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<SeedExportForm>,
) -> Response {
    let session = match state.admin_auth.authenticate(&headers).await {
        Ok(Some(session)) => session,
        _ => return StatusCode::UNAUTHORIZED.into_response(),
    };
    if !csrf_matches(&session, &form.csrf_token) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if !state.admin_auth.verify_password(&form.password) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match RootSecret::load(&state.config.root_secret_file) {
        Ok(secret) => (
            [
                (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
                (
                    header::CONTENT_DISPOSITION,
                    "attachment; filename=lnaddrd-root-seed.txt",
                ),
                (header::CACHE_CONTROL, "no-store"),
            ],
            format!("{}\n", secret.expose_hex()),
        )
            .into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

pub async fn address_retry_submit(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<AddressActionForm>,
) -> Response {
    let session = match state.admin_auth.authenticate(&headers).await {
        Ok(Some(value)) => value,
        _ => return StatusCode::UNAUTHORIZED.into_response(),
    };
    if !csrf_matches(&session, &form.csrf_token) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let managed = match state
        .repository
        .get_address_for_management(&form.domain, &form.username)
        .await
    {
        Ok(Some(value)) => value,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let Some(event_id) = managed.backup_event_id else {
        return configuration_error(anyhow::anyhow!("Address has no backup event"));
    };
    match state.repository.retry_event_now(&event_id).await {
        Ok(()) => {
            Html(html! { p role="status" { "Backup queued for immediate retry." } }.into_string())
                .into_response()
        }
        Err(error) => configuration_error(error),
    }
}

pub async fn address_delete_submit(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<AddressActionForm>,
) -> Response {
    let session = match state.admin_auth.authenticate(&headers).await {
        Ok(Some(value)) => value,
        _ => return StatusCode::UNAUTHORIZED.into_response(),
    };
    if !csrf_matches(&session, &form.csrf_token) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let managed = match state
        .repository
        .get_address_for_management(&form.domain, &form.username)
        .await
    {
        Ok(Some(value)) => value,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    if managed.state != "active" {
        return configuration_error(anyhow::anyhow!("Only active addresses can be deleted"));
    }
    let now = match u64::try_from(unix_now().unwrap_or_default()) {
        Ok(value) => value,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let record = AddressRecord::tombstone(
        &state.keys,
        managed.address,
        managed.revision + 1,
        managed.created_at,
        now,
        UpdatedBy::Admin,
    );
    let event = match BackupCodec::new(&state.keys).encode_address(&record) {
        Ok(value) => value,
        Err(error) => return configuration_error(error),
    };
    if let Err(error) = state
        .repository
        .stage_deletion(&form.domain, &form.username, &event)
        .await
    {
        return configuration_error(error);
    }
    match state.publisher.publish(&event).await {
        Ok(publication) => {
            if let Err(error) = state
                .repository
                .record_publication(&event.id.to_string(), &publication)
                .await
            {
                return configuration_error(error);
            }
            if let Err(error) = state
                .repository
                .acknowledge_event(&event.id.to_string())
                .await
            {
                return configuration_error(error);
            }
            Html(
                html! { p role="status" { "Address deleted after tombstone acknowledgement." } }
                    .into_string(),
            )
            .into_response()
        }
        Err(error) => {
            let _ = state
                .repository
                .fail_event(&event.id.to_string(), &error.to_string())
                .await;
            Html(
                html! { p role="status" { "Deletion staged; waiting for relay acknowledgement." } }
                    .into_string(),
            )
            .into_response()
        }
    }
}

pub async fn restore_dry_run_submit(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<LogoutForm>,
) -> Response {
    let session = match state.admin_auth.authenticate(&headers).await {
        Ok(Some(value)) => value,
        _ => return StatusCode::UNAUTHORIZED.into_response(),
    };
    if !csrf_matches(&session, &form.csrf_token) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let directory = match tempfile::tempdir() {
        Ok(value) => value,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let path = directory.path().join("restore-check.sqlite3");
    let repository = match SqlitePaymentAddressRepository::new(path.to_str().unwrap_or_default()) {
        Ok(value) => value,
        Err(error) => return configuration_error(error),
    };
    match restore::restore(&repository, state.nostr.as_ref(), state.keys.clone(), &state.config.domains, true).await {
        Ok(summary) => Html(html! { p role="status" { "Remote backup: " (summary.active_addresses) " active, " (summary.tombstones) " tombstones, configuration revision " (summary.configuration_revision) "." } }.into_string()).into_response(),
        Err(error) => configuration_error(error),
    }
}

pub async fn reserved_name_submit(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<ReservedNameForm>,
) -> Response {
    let session = match state.admin_auth.authenticate(&headers).await {
        Ok(Some(session)) => session,
        _ => return StatusCode::UNAUTHORIZED.into_response(),
    };
    if !csrf_matches(&session, &form.csrf_token) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let domain = match form.domain.parse::<Domain>() {
        Ok(domain) => domain,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let username = match form.username.parse::<Username>() {
        Ok(username) => username,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let update = match state
        .configuration_manager
        .set_reserved(domain, username, form.reserved)
        .await
    {
        Ok(update) => update,
        Err(error) => {
            let markup = html! { p role="alert" { (error) } };
            return (StatusCode::CONFLICT, Html(markup.into_string())).into_response();
        }
    };
    let configuration = match state.configuration_manager.current().await {
        Ok(configuration) => configuration,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    if update.active {
        if let Err(error) = publish_announcement(&state, &configuration).await {
            return configuration_error(error);
        }
    }
    let message = if update.active {
        format!("Configuration revision {} is active.", update.revision)
    } else {
        format!(
            "Configuration revision {} is waiting for relay acknowledgement.",
            update.revision
        )
    };
    Html(configuration_markup(&configuration, &session.csrf_token, Some(&message)).into_string())
        .into_response()
}

pub async fn payment_policy_submit(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<PaymentPolicyForm>,
) -> Response {
    let session = match state.admin_auth.authenticate(&headers).await {
        Ok(Some(session)) => session,
        _ => return StatusCode::UNAUTHORIZED.into_response(),
    };
    if !csrf_matches(&session, &form.csrf_token) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let domain = match form.domain.parse::<Domain>() {
        Ok(domain) => domain,
        Err(error) => return configuration_error(error),
    };
    let policy = if form.enabled {
        let policy = match parse_policy(&form.destination, &form.tiers) {
            Ok(policy) => policy,
            Err(error) => return configuration_error(error),
        };
        if let Err(error) = PaymentClient::default().validate_policy(&policy).await {
            return configuration_error(error);
        }
        Some(policy)
    } else {
        None
    };
    let update = match state
        .configuration_manager
        .set_payment_policy(domain, policy)
        .await
    {
        Ok(update) => update,
        Err(error) => return configuration_error(error),
    };
    let configuration = match state.configuration_manager.current().await {
        Ok(configuration) => configuration,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    if update.active {
        if let Err(error) = publish_announcement(&state, &configuration).await {
            return configuration_error(error);
        }
    }
    let message = if update.active {
        format!("Configuration revision {} is active.", update.revision)
    } else {
        format!(
            "Configuration revision {} is waiting for relay acknowledgement.",
            update.revision
        )
    };
    Html(configuration_markup(&configuration, &session.csrf_token, Some(&message)).into_string())
        .into_response()
}

pub async fn logout_submit(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<LogoutForm>,
) -> Response {
    let session = match state.admin_auth.authenticate(&headers).await {
        Ok(Some(session)) => session,
        _ => return StatusCode::UNAUTHORIZED.into_response(),
    };
    if !csrf_matches(&session, &form.csrf_token) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if state.admin_auth.logout(&session).await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let expired =
        format!("{SESSION_COOKIE}=; Path=/admin; Max-Age=0; Secure; HttpOnly; SameSite=Strict");
    (
        [(header::SET_COOKIE, expired)],
        Redirect::to("/admin/login"),
    )
        .into_response()
}

fn configuration_markup(
    configuration: &ServiceConfigurationRecord,
    csrf_token: &str,
    message: Option<&str>,
) -> maud::Markup {
    html! {
        @if let Some(message) = message { p role="status" { (message) } }
        h2 { "Reserved names" }
        @for (domain, domain_configuration) in &configuration.domains {
            section {
                h3 { (domain) }
                h4 { "Registration pricing" }
                @let destination = domain_configuration.payment_policy.as_ref()
                    .and_then(|policy| Destination::try_from(policy.destination.clone()).ok())
                    .map(|value| value.to_string()).unwrap_or_default();
                @let tiers = domain_configuration.payment_policy.as_ref().map(|policy| {
                    policy.tiers.iter().map(|tier| format!("{}={}", tier.max_length, tier.price_msat))
                        .collect::<Vec<_>>().join("\n")
                }).unwrap_or_default();
                form method="post" action="/admin/payment-policy"
                    hx-post="/admin/payment-policy" hx-target="#configuration" hx-swap="innerHTML" {
                    input type="hidden" name="csrf_token" value=(csrf_token);
                    input type="hidden" name="domain" value=(domain);
                    label {
                        input type="checkbox" name="enabled" value="true"
                            checked[domain_configuration.payment_policy.is_some()];
                        " Require payment"
                    }
                    label { " Recipient " input name="destination" value=(destination); }
                    label {
                        " Tiers (max_length=price_msat, one per line)"
                        textarea name="tiers" { (tiers) }
                    }
                    p { "Saving probes the recipient with an unpaid invoice and requires LUD-21 verification." }
                    button type="submit" { "Save pricing" }
                }
                h4 { "Reserved names" }
                ul {
                    @for username in &domain_configuration.reserved_names {
                        li {
                            (username)
                            form method="post" action="/admin/reserved" style="display:inline"
                                hx-post="/admin/reserved" hx-target="#configuration" hx-swap="innerHTML" {
                                input type="hidden" name="csrf_token" value=(csrf_token);
                                input type="hidden" name="domain" value=(domain);
                                input type="hidden" name="username" value=(username);
                                input type="hidden" name="reserved" value="false";
                                button type="submit" { "Unreserve" }
                            }
                        }
                    }
                }
                form method="post" action="/admin/reserved"
                    hx-post="/admin/reserved" hx-target="#configuration" hx-swap="innerHTML" {
                    input type="hidden" name="csrf_token" value=(csrf_token);
                    input type="hidden" name="domain" value=(domain);
                    input type="hidden" name="reserved" value="true";
                    label { "Reserve username " input name="username" required; }
                    button type="submit" { "Reserve" }
                }
            }
        }
    }
}

fn configuration_error(error: anyhow::Error) -> Response {
    let markup = html! { p role="alert" { (error) } };
    (StatusCode::BAD_REQUEST, Html(markup.into_string())).into_response()
}

async fn publish_announcement(
    state: &AppState,
    configuration: &ServiceConfigurationRecord,
) -> Result<()> {
    let now = u64::try_from(unix_now()?)?;
    if let Some(event) = build_event(&state.config, configuration, &state.keys, now)? {
        state.publisher.publish(&event).await?;
    }
    Ok(())
}

fn csrf_matches(session: &AdminSession, candidate: &str) -> bool {
    session
        .csrf_token
        .as_bytes()
        .ct_eq(candidate.as_bytes())
        .unwrap_u8()
        == 1
}

fn login_markup(error: Option<&str>) -> String {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="UTF-8";
                meta name="viewport" content="width=device-width, initial-scale=1.0";
                title { "lnaddrd admin login" }
            }
            body {
                main {
                    h1 { "Administrator login" }
                    @if let Some(error) = error { p { (error) } }
                    form method="post" action="/admin/login" {
                        label for="password" { "Password" }
                        input id="password" name="password" type="password" required autofocus;
                        button type="submit" { "Log in" }
                    }
                }
            }
        }
    }
    .into_string()
}

fn load_or_create_password(path: &Path) -> Result<String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            ensure!(
                metadata.file_type().is_file(),
                "Admin password must be a regular file"
            );
            ensure!(
                !metadata.file_type().is_symlink(),
                "Admin password must not be a symlink"
            );
            ensure_private_permissions(&metadata, path)?;
            let mut password = String::new();
            OpenOptions::new()
                .read(true)
                .open(path)?
                .read_to_string(&mut password)?;
            validate_password(password.trim())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => create_password(path),
        Err(error) => Err(error.into()),
    }
}

fn create_password(path: &Path) -> Result<String> {
    if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
        create_private_directory(parent)?;
    }
    let password = random_hex();
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(mut file) => {
            writeln!(file, "{password}")?;
            file.sync_all()?;
            info!(path=%path.display(), "Generated administrator password; read it from this file to complete setup");
            Ok(password)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            load_or_create_password(path)
        }
        Err(error) => Err(error.into()),
    }
}

fn validate_password(password: &str) -> Result<String> {
    ensure!(!password.is_empty(), "Admin password must not be empty");
    ensure!(password.len() <= 4096, "Admin password is too long");
    Ok(password.to_owned())
}

fn create_private_directory(path: &Path) -> Result<()> {
    if !path.exists() {
        fs::create_dir_all(path).with_context(|| format!("Failed to create {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        }
    }
    ensure!(path.is_dir(), "Admin-password parent is not a directory");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            fs::metadata(path)?.permissions().mode() & 0o077 == 0,
            "Admin-password directory {} must not be accessible by group or others",
            path.display()
        );
    }
    Ok(())
}

fn random_hex() -> String {
    let mut bytes = [0_u8; 32];
    OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .map(str::trim)
        .find_map(|cookie| {
            cookie
                .strip_prefix(&format!("{name}="))
                .map(ToOwned::to_owned)
        })
}

fn unix_now() -> Result<i64> {
    Ok(i64::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
    )?)
}

fn ensure_private_permissions(metadata: &fs::Metadata, path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            metadata.permissions().mode() & 0o077 == 0,
            "Admin password at {} is accessible by group or others",
            path.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auth() -> (tempfile::TempDir, AdminAuth) {
        let directory = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        }
        let repository = SqlitePaymentAddressRepository::new(
            directory.path().join("db.sqlite3").to_str().unwrap(),
        )
        .unwrap();
        let auth =
            AdminAuth::load_or_create(&directory.path().join("password"), repository).unwrap();
        (directory, auth)
    }

    #[tokio::test]
    async fn login_and_session_authentication() {
        let (directory, auth) = auth();
        let password = fs::read_to_string(directory.path().join("password")).unwrap();
        let session = auth.login(password.trim()).await.unwrap().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            format!("{SESSION_COOKIE}={}", session.token)
                .parse()
                .unwrap(),
        );
        assert!(auth.authenticate(&headers).await.unwrap().is_some());
        auth.logout(&session).await.unwrap();
        assert!(auth.authenticate(&headers).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn replacing_password_invalidates_existing_sessions() {
        let (directory, auth) = auth();
        let path = directory.path().join("password");
        let password = fs::read_to_string(&path).unwrap();
        let session = auth.login(password.trim()).await.unwrap().unwrap();

        fs::write(&path, format!("{}\n", "12".repeat(32))).unwrap();
        let replacement = AdminAuth::load_or_create(&path, auth.repository.clone()).unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            format!("{SESSION_COOKIE}={}", session.token)
                .parse()
                .unwrap(),
        );
        assert!(replacement.authenticate(&headers).await.unwrap().is_none());
    }
}
