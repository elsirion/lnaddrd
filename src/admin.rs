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
    extract::{Path as AxumPath, State},
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
use crate::nostr::codec::{
    AddressRecord, BackupCodec, ServiceConfigurationRecord, ServiceProfileRecord, UpdatedBy,
};
use crate::nostr::restore;
use crate::payment::{PaymentClient, parse_policy};
use crate::{AppState, crypto::RootSecret, repository::sqlite::SqlitePaymentAddressRepository};

pub(crate) const SESSION_COOKIE: &str = "lnaddrd_admin";
const SESSION_DURATION: Duration = Duration::from_secs(12 * 60 * 60);

const ADMIN_POLICY_JS: &str = r#"
(function () {
  function pending(form, message) {
    var status = form.querySelector('[data-recipient-validation]');
    status.innerHTML = '<span class="inline-flex items-center gap-2 text-sm text-gray-500"><svg class="h-4 w-4 animate-spin" viewBox="0 0 24 24" fill="none"><circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"></circle><path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8v4a4 4 0 00-4 4H4z"></path></svg>' + message + '</span>';
    status.dataset.valid = 'false';
    form.querySelector('[data-save-pricing]').disabled = true;
  }

  function syncTiers(form, requestCheck) {
    var rows = Array.from(form.querySelectorAll('[data-tier-row]'));
    rows.sort(function (a, b) {
      return Number(a.querySelector('[data-tier-length]').value) - Number(b.querySelector('[data-tier-length]').value);
    });
    rows.forEach(function (row) { form.querySelector('[data-tier-list]').appendChild(row); });
    var error = form.querySelector('[data-tier-error]');
    var values = [];
    var previousLength = 0;
    var previousPrice = Number.MAX_SAFE_INTEGER;
    var message = '';
    rows.forEach(function (row) {
      var lengthValue = row.querySelector('[data-tier-length]').value;
      var priceValue = row.querySelector('[data-tier-price]').value;
      var length = Number(lengthValue);
      var priceSats = Number(priceValue);
      var price = Math.round(priceSats * 1000);
      if (lengthValue === '' || priceValue === '') message ||= 'Complete both fields for every tier.';
      if (!Number.isInteger(length) || length < 1 || length > 64) message ||= 'Maximum length must be a whole number from 1 to 64.';
      if (!Number.isFinite(priceSats) || priceSats < 0 || !Number.isSafeInteger(price) || Math.abs(price / 1000 - priceSats) > 0.0000001) message ||= 'Price must be a non-negative sat amount with at most three decimals.';
      if (length <= previousLength) message ||= 'Tier lengths must be strictly increasing.';
      if (price > previousPrice) message ||= 'Prices must stay the same or decrease for longer names.';
      previousLength = length;
      previousPrice = price;
      values.push(length + '=' + price);
    });
    if (!rows.length) message = 'Add at least one pricing tier.';
    form.querySelector('[data-tiers-value]').value = values.join('\n');
    error.textContent = message;
    error.classList.toggle('hidden', !message);
    if (form.querySelector('[data-payment-enabled]').checked) {
      form.querySelector('[data-save-pricing]').disabled = true;
      if (!message && requestCheck && form.querySelector('[data-payment-destination]').value.trim()) {
        pending(form, 'Checking recipient and LUD-21 support…');
        htmx.trigger(form.querySelector('[data-payment-destination]'), 'pricing-tiers-changed');
      }
    }
    return !message;
  }

  function addTier(form) {
    var row = document.createElement('div');
    row.dataset.tierRow = '';
    row.className = 'grid gap-3 rounded-lg border border-gray-200 bg-gray-50 p-3 sm:grid-cols-[1fr_1fr_auto] sm:items-end';
    row.innerHTML = '<div><label class="mb-2 block text-xs font-medium text-gray-700">Username shorter than</label><input data-tier-length type="number" min="1" max="64" step="1" required value="1" class="block w-full rounded-lg border border-gray-300 bg-white p-2.5 text-sm"></div><div><label class="mb-2 block text-xs font-medium text-gray-700">Price (sats)</label><input data-tier-price type="number" min="0" step="0.001" required value="0" class="block w-full rounded-lg border border-gray-300 bg-white p-2.5 text-sm"></div><button type="button" data-remove-tier class="rounded-lg px-3 py-2.5 text-sm font-medium text-red-700 hover:bg-red-50">Remove</button>';
    form.querySelector('[data-tier-list]').appendChild(row);
    row.querySelector('[data-tier-length]').focus();
    syncTiers(form, false);
  }

  function init(root) {
    root.querySelectorAll('[data-payment-policy]:not([data-policy-ready])').forEach(function (form) {
      form.dataset.policyReady = '';
      var enabled = form.querySelector('[data-payment-enabled]');
      var fields = form.querySelector('[data-payment-fields]');
      var destination = form.querySelector('[data-payment-destination]');
      function toggle() {
        fields.classList.toggle('hidden', !enabled.checked);
        if (!enabled.checked) form.querySelector('[data-save-pricing]').disabled = false;
        else if (syncTiers(form, false) && destination.value.trim()) {
          pending(form, 'Checking recipient and LUD-21 support…');
          htmx.trigger(destination, 'pricing-tiers-changed');
        }
      }
      enabled.addEventListener('change', toggle);
      destination.addEventListener('input', function () {
        if (enabled.checked) pending(form, destination.value.trim() ? 'Checking recipient and LUD-21 support…' : 'Enter a recipient to check LUD-21 support.');
      });
      form.querySelector('[data-add-tier]').addEventListener('click', function () { addTier(form); });
      form.querySelector('[data-tier-list]').addEventListener('click', function (event) {
        var button = event.target.closest('[data-remove-tier]');
        if (button) { button.closest('[data-tier-row]').remove(); syncTiers(form, true); }
      });
      form.querySelector('[data-tier-list]').addEventListener('input', function () { syncTiers(form, true); });
      form.addEventListener('submit', function (event) {
        if (enabled.checked && (!syncTiers(form, false) || form.querySelector('[data-recipient-validation] [data-valid="true"]') === null)) {
          event.preventDefault();
          pending(form, 'Complete a successful recipient check before saving.');
        }
      });
      syncTiers(form, false);
      toggle();
    });
  }
  document.addEventListener('DOMContentLoaded', function () { init(document); });
  document.addEventListener('htmx:afterSwap', function (event) {
    init(event.detail.target);
    var status = event.detail.target.closest && event.detail.target.closest('[data-recipient-validation]');
    if (status) {
      var form = status.closest('[data-payment-policy]');
      var valid = status.querySelector('[data-valid="true"]') !== null;
      status.dataset.valid = valid ? 'true' : 'false';
      form.querySelector('[data-save-pricing]').disabled = form.querySelector('[data-payment-enabled]').checked && !valid;
    }
  });
})();
"#;

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
pub struct PaymentPolicyValidationForm {
    csrf_token: String,
    destination: String,
    tiers: String,
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

#[derive(Debug, Deserialize)]
pub struct ProfileForm {
    pub csrf_token: String,
    #[serde(default)]
    pub about: String,
    #[serde(default)]
    pub contact: String,
    #[serde(default)]
    pub terms_url: String,
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
    let income = state
        .repository
        .paid_income_by_domain()
        .await
        .unwrap_or_default();
    let relays = state.repository.relay_health().await.unwrap_or_default();
    let replication = state
        .repository
        .relay_replication()
        .await
        .unwrap_or_default();
    let total_addresses = addresses.len();
    let markup = html! {
        (DOCTYPE)
        html lang="en" {
            (admin_head("Dashboard"))
            body class="bg-gray-50 text-gray-900" {
                (admin_nav(&session.csrf_token))
                main class="mx-auto max-w-7xl p-4 sm:p-6 lg:p-8 space-y-8" {
                    div { h1 class="text-3xl font-bold tracking-tight" { "Dashboard" } p class="mt-1 text-sm text-gray-500" { "Service overview and backup health" } }
                    div class="grid gap-4 sm:grid-cols-3" {
                        (stat_card("Addresses", &total_addresses.to_string()))
                        (stat_card("Domains", &configuration.domains.len().to_string()))
                        (stat_card("Pending events", &pending.to_string()))
                    }
                    div class="rounded-lg border border-gray-200 bg-white px-5 py-4 text-sm shadow-sm" {
                        span class="font-medium text-gray-700" { "Service public key" }
                        code class="mt-1 block break-all text-xs text-gray-500" { (state.keys.service_public_key()) }
                    }
                    @let profile = configuration.profile.clone().unwrap_or(ServiceProfileRecord { about: None, contact: None, terms_url: None });
                    section class="rounded-lg border border-gray-200 bg-white p-5 shadow-sm" {
                        h2 class="text-xl font-semibold" { "Public profile" }
                        p class="mt-1 text-sm text-gray-500" { "Published in the Nostr service announcement so marketplaces can present this operator." }
                        form method="post" action="/admin/profile" hx-post="/admin/profile" hx-target="#profile-result" class="mt-4 space-y-4 max-w-2xl" {
                            input type="hidden" name="csrf_token" value=(session.csrf_token);
                            div { label for="profile-about" class=(label_class()) { "About (max 500 characters)" }
                                textarea id="profile-about" name="about" rows="3" class=(input_class()) { (profile.about.clone().unwrap_or_default()) } }
                            div { label for="profile-contact" class=(label_class()) { "Contact (npub)" }
                                input id="profile-contact" name="contact" value=(profile.contact.clone().unwrap_or_default()) class=(input_class()); }
                            div { label for="profile-terms" class=(label_class()) { "Terms URL (HTTPS)" }
                                input id="profile-terms" name="terms_url" value=(profile.terms_url.clone().unwrap_or_default()) class=(input_class()); }
                            button type="submit" class=(primary_button()) { "Save profile" }
                        }
                        div id="profile-result" class="mt-3" {}
                    }
                    section class="rounded-lg border border-gray-200 bg-white shadow-sm" {
                        div class="border-b border-gray-200 p-5" { h2 class="text-xl font-semibold" { "Domains" } }
                        div class="overflow-x-auto" { table class="w-full text-left text-sm text-gray-600" {
                            thead class="bg-gray-50 text-xs uppercase text-gray-700" { tr {
                                th class="px-6 py-3" { "Domain" } th class="px-6 py-3 text-right" { "Addresses" }
                                th class="px-6 py-3 text-right" { "Income" } th class="px-6 py-3 text-right" { "Actions" }
                            } }
                            tbody {
                                @for domain in configuration.domains.keys() {
                                    @let count = addresses.iter().filter(|address| address.domain == domain.as_str()).count();
                                    @let earned = income.get(domain.as_str()).copied().unwrap_or_default();
                                    tr class="border-t border-gray-200 bg-white" {
                                        th scope="row" class="whitespace-nowrap px-6 py-4 font-medium text-gray-900" { (domain) }
                                        td class="px-6 py-4 text-right" { (count) }
                                        td class="px-6 py-4 text-right font-mono" { (format_msat(earned)) }
                                        td class="px-6 py-4" { div class="flex justify-end gap-2" {
                                            a href=(format!("/admin/domains/{domain}/addresses")) title="List addresses" aria-label=(format!("List addresses for {domain}")) class="rounded-lg p-2 text-blue-700 hover:bg-blue-50" { (list_icon()) }
                                            a href=(format!("/admin/domains/{domain}/settings")) title="Edit settings" aria-label=(format!("Edit settings for {domain}")) class="rounded-lg p-2 text-gray-700 hover:bg-gray-100" { (wrench_icon()) }
                                        } }
                                    }
                                }
                            }
                        } }
                    }
                    section class="rounded-lg border border-gray-200 bg-white p-5 shadow-sm" {
                        div class="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between" {
                            div { h2 class="text-xl font-semibold" { "Nostr replication" } p class="mt-1 text-sm text-gray-500" { "Current backup events confirmed by each relay. Records are refreshed every six hours." } }
                            form method="post" action="/admin/restore-dry-run" hx-post="/admin/restore-dry-run" hx-target="#admin-action-result" {
                                input type="hidden" name="csrf_token" value=(session.csrf_token);
                                button type="submit" class=(secondary_button()) { "Compare remote backup" }
                            }
                        }
                        div id="admin-action-result" class="mt-4" {}
                        div class="mt-4 overflow-x-auto" { table class="w-full text-left text-sm text-gray-600" {
                            thead class="bg-gray-50 text-xs uppercase text-gray-700" { tr { th class="px-4 py-3" { "Relay" } th class="px-4 py-3 text-right" { "Confirmed events" } th class="px-4 py-3" { "Last success" } th class="px-4 py-3" { "Last error" } } }
                            tbody { @for relay in relays {
                                @let count = replication.iter().find(|entry| entry.relay_url == relay.relay_url).map(|entry| entry.confirmed_events).unwrap_or_default();
                                tr class="border-t border-gray-200" { td class="px-4 py-3 font-mono text-xs" { (relay.relay_url) } td class="px-4 py-3 text-right" { (count) } td class="px-4 py-3" { (format_timestamp(relay.last_success_at)) } td class="px-4 py-3 text-red-600" { (relay.last_error.unwrap_or_default()) } }
                            } }
                        } }
                    }
                    section class="rounded-lg border border-amber-200 bg-amber-50 p-5" {
                        h2 class="text-xl font-semibold text-amber-950" { "Root seed backup" }
                        p class="mt-1 text-sm text-amber-800" { "The root seed is the only irreplaceable service state. Export it only to secure offline storage." }
                        form method="post" action="/admin/seed/export" class="mt-4 flex max-w-xl flex-col gap-3 sm:flex-row sm:items-end" {
                            input type="hidden" name="csrf_token" value=(session.csrf_token);
                            div class="grow" { label for="seed-password" class=(label_class()) { "Confirm admin password" } input id="seed-password" type="password" name="password" required class=(input_class()); }
                            button type="submit" class=(primary_button()) { "Export root seed" }
                        }
                    }
                }
            }
        }
    };
    Html(markup.into_string()).into_response()
}

pub async fn domain_addresses(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(domain): AxumPath<String>,
) -> Response {
    let session = match state.admin_auth.authenticate(&headers).await {
        Ok(Some(session)) => session,
        Ok(None) => return Redirect::to("/admin/login").into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    if !state
        .config
        .domains
        .iter()
        .any(|configured| configured == &domain)
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    let addresses = match state.repository.admin_addresses_for_domain(&domain).await {
        Ok(addresses) => addresses,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let markup = html! {
        (DOCTYPE)
        html lang="en" {
            (admin_head(&format!("{domain} addresses")))
            body class="bg-gray-50 text-gray-900" {
                (admin_nav(&session.csrf_token))
                main class="mx-auto max-w-7xl p-4 sm:p-6 lg:p-8 space-y-6" {
                    div class="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between" {
                        div { a href="/admin" class="text-sm font-medium text-blue-700 hover:underline" { "← Dashboard" } h1 class="mt-2 text-3xl font-bold tracking-tight" { (domain) " addresses" } p class="mt-1 text-sm text-gray-500" { (addresses.len()) " registered addresses" } }
                        a href=(format!("/admin/domains/{domain}/settings")) class=(secondary_button()) { (wrench_icon()) span class="ml-2" { "Domain settings" } }
                    }
                    div id="admin-action-result" {}
                    section class="overflow-hidden rounded-lg border border-gray-200 bg-white shadow-sm" {
                        div class="overflow-x-auto" { table class="w-full text-left text-sm text-gray-600" {
                            thead class="bg-gray-50 text-xs uppercase text-gray-700" { tr { th class="px-5 py-3" { "Address" } th class="px-5 py-3" { "Destination" } th class="px-5 py-3" { "State" } th class="px-5 py-3 text-right" { "Revision" } th class="px-5 py-3 text-right" { "Actions" } } }
                            tbody {
                                @if addresses.is_empty() { tr { td colspan="5" class="px-5 py-10 text-center text-gray-500" { "No addresses for this domain." } } }
                                @for address in addresses {
                                    tr class="border-t border-gray-200 align-top" {
                                        th scope="row" class="whitespace-nowrap px-5 py-4 font-medium text-gray-900" { (address.username) "@" (address.domain) }
                                        td class="max-w-md break-all px-5 py-4 font-mono text-xs" { (address.destination) }
                                        td class="px-5 py-4" { span class=(state_badge(&address.state)) { (address.state) } }
                                        td class="px-5 py-4 text-right" { (address.revision) }
                                        td class="px-5 py-4" { div class="flex justify-end gap-2" {
                                            @if address.backup_event_id.is_some() { form method="post" action="/admin/address/retry" hx-post="/admin/address/retry" hx-target="#admin-action-result" {
                                                input type="hidden" name="csrf_token" value=(session.csrf_token); input type="hidden" name="domain" value=(address.domain); input type="hidden" name="username" value=(address.username);
                                                button type="submit" class=(secondary_button()) { "Retry backup" }
                                            } }
                                            form method="post" action="/admin/address/delete" hx-post="/admin/address/delete" hx-target="#admin-action-result" hx-confirm=(format!("Delete {}@{}?", address.username, address.domain)) {
                                                input type="hidden" name="csrf_token" value=(session.csrf_token); input type="hidden" name="domain" value=(address.domain); input type="hidden" name="username" value=(address.username);
                                                button type="submit" class=(danger_button()) { "Delete" }
                                            }
                                        } }
                                    }
                                }
                            }
                        } }
                    }
                }
            }
        }
    };
    Html(markup.into_string()).into_response()
}

pub async fn domain_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(domain): AxumPath<String>,
) -> Response {
    let session = match state.admin_auth.authenticate(&headers).await {
        Ok(Some(session)) => session,
        Ok(None) => return Redirect::to("/admin/login").into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let parsed_domain = match domain.parse::<Domain>() {
        Ok(domain) => domain,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    let configuration = match state.configuration_manager.current().await {
        Ok(configuration) if configuration.domains.contains_key(&parsed_domain) => configuration,
        Ok(_) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let markup = html! {
        (DOCTYPE)
        html lang="en" {
            (admin_head(&format!("{domain} settings")))
            body class="bg-gray-50 text-gray-900" {
                (admin_nav(&session.csrf_token))
                main class="mx-auto max-w-4xl p-4 sm:p-6 lg:p-8" {
                    a href="/admin" class="text-sm font-medium text-blue-700 hover:underline" { "← Dashboard" }
                    div class="mt-2 flex items-center gap-3" { (wrench_icon()) h1 class="text-3xl font-bold tracking-tight" { (domain) " settings" } }
                    section id="configuration" class="mt-6" { (configuration_markup(&configuration, &session.csrf_token, Some(&domain), None)) }
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
    let domain = match form.domain.parse::<Domain>() {
        Ok(domain) => domain,
        Err(error) => return configuration_error(error),
    };
    let username = match form.username.parse::<Username>() {
        Ok(username) => username,
        Err(error) => return configuration_error(error),
    };
    let reservation = match state
        .configuration_manager
        .set_reserved(domain, username, true)
        .await
    {
        Ok(update) => update,
        Err(error) => return configuration_error(error),
    };
    if !reservation.active {
        return Html(
            html! { p role="status" { "Address was not deleted because its reservation is still waiting for relay acknowledgement. Retry deletion after the configuration backup succeeds." } }
                .into_string(),
        )
        .into_response();
    }
    let configuration = match state.configuration_manager.current().await {
        Ok(configuration) => configuration,
        Err(error) => return configuration_error(error),
    };
    if let Err(error) = publish_announcement(&state, &configuration).await {
        return configuration_error(error);
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
    Html(
        configuration_markup(
            &configuration,
            &session.csrf_token,
            Some(&form.domain),
            Some(&message),
        )
        .into_string(),
    )
    .into_response()
}

pub async fn profile_submit(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<ProfileForm>,
) -> Response {
    let session = match state.admin_auth.authenticate(&headers).await {
        Ok(Some(session)) => session,
        _ => return StatusCode::UNAUTHORIZED.into_response(),
    };
    if !csrf_matches(&session, &form.csrf_token) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let field = |value: String| Some(value.trim().to_owned()).filter(|value| !value.is_empty());
    let profile = ServiceProfileRecord {
        about: field(form.about),
        contact: field(form.contact),
        terms_url: field(form.terms_url),
    };
    let update = match state.configuration_manager.set_profile(Some(profile)).await {
        Ok(update) => update,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Html(
                    html! { p role="alert" class="text-sm text-red-600" { (error) } }.into_string(),
                ),
            )
                .into_response();
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
        format!(
            "Profile saved; configuration revision {} is active and the announcement was republished.",
            update.revision
        )
    } else {
        format!(
            "Profile saved; revision {} is waiting for relay acknowledgement.",
            update.revision
        )
    };
    Html(html! { p role="status" class="text-sm text-green-700" { (message) } }.into_string())
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
    Html(
        configuration_markup(
            &configuration,
            &session.csrf_token,
            Some(&form.domain),
            Some(&message),
        )
        .into_string(),
    )
    .into_response()
}

pub async fn payment_policy_validate(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<PaymentPolicyValidationForm>,
) -> Response {
    let session = match state.admin_auth.authenticate(&headers).await {
        Ok(Some(session)) => session,
        _ => return StatusCode::UNAUTHORIZED.into_response(),
    };
    if !csrf_matches(&session, &form.csrf_token) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let result = match parse_policy(&form.destination, &form.tiers) {
        Ok(policy) => PaymentClient::default().validate_policy(&policy).await,
        Err(error) => Err(error),
    };
    let markup = match result {
        Ok(()) => {
            html! { span data-valid="true" class="inline-flex items-center gap-1.5 text-sm text-green-700" { "✓ LUD-21 verification supported" } }
        }
        Err(error) => {
            html! { span data-valid="false" class="text-sm text-red-700" { "✕ " (error) } }
        }
    };
    Html(markup.into_string()).into_response()
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
    selected_domain: Option<&str>,
    message: Option<&str>,
) -> maud::Markup {
    html! {
        @if let Some(message) = message { div role="status" class="mb-5 rounded-lg border border-green-200 bg-green-50 p-4 text-sm text-green-800" { (message) } }
        @for (domain, domain_configuration) in &configuration.domains {
            @if selected_domain.is_none_or(|selected| selected == domain.as_str()) {
            div class="space-y-6" {
                section class="rounded-lg border border-gray-200 bg-white p-6 shadow-sm" {
                h2 class="text-xl font-semibold" { "Registration pricing" }
                p class="mt-1 text-sm text-gray-500" { "Optionally charge based on username length and verify payment through LUD-21." }
                @let destination = domain_configuration.payment_policy.as_ref()
                    .and_then(|policy| Destination::try_from(policy.destination.clone()).ok())
                    .map(|value| value.to_string()).unwrap_or_default();
                @let payment_fields_class = if domain_configuration.payment_policy.is_some() { "space-y-5" } else { "hidden space-y-5" };
                form method="post" action="/admin/payment-policy"
                    class="mt-5 space-y-4" data-payment-policy hx-post="/admin/payment-policy" hx-target="#configuration" hx-swap="innerHTML" {
                    input type="hidden" name="csrf_token" value=(csrf_token);
                    input type="hidden" name="domain" value=(domain);
                    label class="inline-flex cursor-pointer items-center gap-3" {
                        input type="checkbox" name="enabled" value="true" data-payment-enabled class="h-4 w-4 rounded border-gray-300 text-blue-600 focus:ring-blue-500"
                            checked[domain_configuration.payment_policy.is_some()];
                        span class="text-sm font-medium text-gray-900" { "Require payment" }
                    }
                    div data-payment-fields class=(payment_fields_class) {
                    div { label for="pricing-destination" class=(label_class()) { "Recipient LNURL or Lightning Address" }
                        input id="pricing-destination" name="destination" value=(destination) class=(input_class()) data-payment-destination
                            hx-post="/admin/payment-policy/validate" hx-trigger="input changed delay:600ms, pricing-tiers-changed delay:300ms" hx-include="closest form" hx-target="#recipient-validation" hx-swap="innerHTML" hx-sync="this:replace";
                        div id="recipient-validation" class="mt-2 min-h-5 text-sm text-gray-500" data-recipient-validation { "Enter a recipient to check LUD-21 support." }
                    }
                    div { div class="flex items-center justify-between gap-4" { div { label class=(label_class()) { "Pricing tiers" } p class="text-xs text-gray-500" { "Shorter names must not cost less than longer names." } } button type="button" data-add-tier class=(secondary_button()) { "+ Add tier" } }
                        div data-tier-list class="mt-3 space-y-3" {
                            @if let Some(policy) = &domain_configuration.payment_policy {
                                @for tier in &policy.tiers { (tier_row(Some(tier.max_length), Some(tier.price_msat))) }
                            } @else { (tier_row(Some(1), Some(0))) }
                        }
                        input type="hidden" name="tiers" data-tiers-value;
                        p data-tier-error class="mt-2 hidden text-sm text-red-700" {}
                    }
                    p class="text-sm text-gray-500" { "The check requests an unpaid invoice in the lowest configured amount and verifies its LUD-21 endpoint." }
                    }
                    div { button type="submit" data-save-pricing class=(primary_button()) disabled[domain_configuration.payment_policy.is_some()] { "Save pricing" } }
                }
                }
                section class="rounded-lg border border-gray-200 bg-white p-6 shadow-sm" {
                h2 class="text-xl font-semibold" { "Reserved names" }
                p class="mt-1 text-sm text-gray-500" { "Prevent selected usernames from being registered publicly." }
                ul class="mt-5 divide-y divide-gray-200 rounded-lg border border-gray-200" {
                    @for username in &domain_configuration.reserved_names {
                        li class="flex items-center justify-between gap-4 px-4 py-3" {
                            code class="text-sm font-medium" { (username) }
                            form method="post" action="/admin/reserved"
                                hx-post="/admin/reserved" hx-target="#configuration" hx-swap="innerHTML" {
                                input type="hidden" name="csrf_token" value=(csrf_token);
                                input type="hidden" name="domain" value=(domain);
                                input type="hidden" name="username" value=(username);
                                input type="hidden" name="reserved" value="false";
                                button type="submit" class=(danger_button()) { "Unreserve" }
                            }
                        }
                    }
                }
                form method="post" action="/admin/reserved"
                    class="mt-5 flex flex-col gap-3 sm:flex-row sm:items-end" hx-post="/admin/reserved" hx-target="#configuration" hx-swap="innerHTML" {
                    input type="hidden" name="csrf_token" value=(csrf_token);
                    input type="hidden" name="domain" value=(domain);
                    input type="hidden" name="reserved" value="true";
                    div class="grow" { label for="reserve-username" class=(label_class()) { "Username" } input id="reserve-username" name="username" required class=(input_class()); }
                    button type="submit" class=(secondary_button()) { "Reserve username" }
                }
                }
            }
            }
        }
    }
}

fn configuration_error(error: anyhow::Error) -> Response {
    let markup = html! { div role="alert" class="rounded-lg border border-red-200 bg-red-50 p-4 text-sm text-red-800" { (error) } };
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

pub(crate) fn admin_head(title: &str) -> maud::Markup {
    html! { head {
        meta charset="UTF-8"; meta name="viewport" content="width=device-width, initial-scale=1.0";
        title { (title) " · lnaddrd admin" }
        link rel="stylesheet" href="/assets/flowbite-1.7.0.min.css";
        script src="/assets/tailwindcss-3.4.17.js" {} script src="/assets/flowbite-1.7.0.min.js" {} script src="/assets/htmx-4.0.0.min.js" {}
        script { (maud::PreEscaped(ADMIN_POLICY_JS)) }
    } }
}

fn tier_row(max_length: Option<u16>, price_msat: Option<u64>) -> maud::Markup {
    let price_sats = price_msat.map(format_sats);
    html! {
        div data-tier-row class="grid gap-3 rounded-lg border border-gray-200 bg-gray-50 p-3 sm:grid-cols-[1fr_1fr_auto] sm:items-end" {
            div { label class="mb-2 block text-xs font-medium text-gray-700" { "Username shorter than" } input data-tier-length type="number" min="1" max="64" step="1" required value=[max_length.map(|value| value.to_string())] class="block w-full rounded-lg border border-gray-300 bg-white p-2.5 text-sm"; }
            div { label class="mb-2 block text-xs font-medium text-gray-700" { "Price (sats)" } input data-tier-price type="number" min="0" step="0.001" required value=[price_sats] class="block w-full rounded-lg border border-gray-300 bg-white p-2.5 text-sm"; }
            button type="button" data-remove-tier class="rounded-lg px-3 py-2.5 text-sm font-medium text-red-700 hover:bg-red-50" { "Remove" }
        }
    }
}

fn format_sats(msat: u64) -> String {
    if msat % 1000 == 0 {
        (msat / 1000).to_string()
    } else {
        format!("{}.{:03}", msat / 1000, msat % 1000)
            .trim_end_matches('0')
            .to_owned()
    }
}

fn admin_nav(csrf_token: &str) -> maud::Markup {
    html! { nav class="border-b border-gray-200 bg-white" { div class="mx-auto flex max-w-7xl items-center justify-between px-4 py-3 sm:px-6 lg:px-8" {
        a href="/admin" class="text-xl font-bold tracking-tight text-gray-900" { "lnaddrd" span class="font-normal text-gray-500" { " admin" } }
        form method="post" action="/admin/logout" { input type="hidden" name="csrf_token" value=(csrf_token); button type="submit" class="rounded-lg px-3 py-2 text-sm font-medium text-gray-700 hover:bg-gray-100" { "Log out" } }
    } } }
}

fn stat_card(label: &str, value: &str) -> maud::Markup {
    html! { div class="rounded-lg border border-gray-200 bg-white p-5 shadow-sm" { p class="text-sm font-medium text-gray-500" { (label) } p class="mt-2 text-3xl font-bold text-gray-900" { (value) } } }
}

pub(crate) fn input_class() -> &'static str {
    "block w-full rounded-lg border border-gray-300 bg-gray-50 p-2.5 text-sm text-gray-900 focus:border-blue-500 focus:ring-blue-500"
}
pub(crate) fn label_class() -> &'static str {
    "mb-2 block text-sm font-medium text-gray-900"
}
pub(crate) fn primary_button() -> &'static str {
    "inline-flex items-center justify-center rounded-lg bg-blue-700 px-4 py-2.5 text-sm font-medium text-white hover:bg-blue-800 focus:outline-none focus:ring-4 focus:ring-blue-300"
}
fn secondary_button() -> &'static str {
    "inline-flex items-center justify-center rounded-lg border border-gray-300 bg-white px-3 py-2 text-sm font-medium text-gray-700 hover:bg-gray-100 focus:outline-none focus:ring-4 focus:ring-gray-200"
}
fn danger_button() -> &'static str {
    "inline-flex items-center justify-center rounded-lg border border-red-200 bg-white px-3 py-2 text-sm font-medium text-red-700 hover:bg-red-50 focus:outline-none focus:ring-4 focus:ring-red-100"
}

fn state_badge(state: &str) -> &'static str {
    if state == "active" {
        "rounded-full bg-green-100 px-2.5 py-1 text-xs font-medium text-green-800"
    } else {
        "rounded-full bg-amber-100 px-2.5 py-1 text-xs font-medium text-amber-800"
    }
}
fn format_msat(value: u64) -> String {
    if value % 1000 == 0 {
        format!("{} sats", value / 1000)
    } else {
        format!("{value} msat")
    }
}
fn format_timestamp(value: Option<i64>) -> String {
    value.map_or_else(
        || "never".to_owned(),
        |timestamp| format!("Unix {timestamp}"),
    )
}

fn list_icon() -> maud::Markup {
    html! { (maud::PreEscaped(r#"<svg class="h-5 w-5" fill="none" viewBox="0 0 24 24" stroke="currentColor" aria-hidden="true"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M8 6h13M8 12h13M8 18h13M3 6h.01M3 12h.01M3 18h.01"/></svg>"#)) }
}
fn wrench_icon() -> maud::Markup {
    html! { (maud::PreEscaped(r#"<svg class="h-5 w-5" fill="none" viewBox="0 0 24 24" stroke="currentColor" aria-hidden="true"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M14.7 6.3a4 4 0 0 0-5 5L3 18v3h3l6.7-6.7a4 4 0 0 0 5-5l-2.4 2.4-3-3 2.4-2.4Z"/></svg>"#)) }
}

fn login_markup(error: Option<&str>) -> String {
    html! {
        (DOCTYPE)
        html lang="en" {
            (admin_head("Login"))
            body class="flex min-h-screen items-center justify-center bg-gray-50 p-4" {
                main class="w-full max-w-md rounded-xl border border-gray-200 bg-white p-8 shadow-sm" {
                    div class="mb-6 text-center" { h1 class="text-2xl font-bold text-gray-900" { "Administrator login" } p class="mt-2 text-sm text-gray-500" { "Sign in to manage this lnaddrd instance." } }
                    @if let Some(error) = error { div role="alert" class="mb-5 rounded-lg border border-red-200 bg-red-50 p-4 text-sm text-red-800" { (error) } }
                    form method="post" action="/admin/login" class="space-y-5" {
                        div { label for="password" class=(label_class()) { "Password" } input id="password" name="password" type="password" required autofocus class=(input_class()); }
                        button type="submit" class=(format!("{} w-full", primary_button())) { "Log in" }
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

    #[test]
    fn pricing_editor_is_structured_and_validation_gated() {
        use crate::nostr::codec::{
            DomainConfigurationRecord, PaymentPolicyRecord, PaymentTierRecord,
        };
        use std::collections::BTreeMap;

        let domain: Domain = "example.com".parse().unwrap();
        let destination: Destination = "receiver@example.net".parse().unwrap();
        let configuration = ServiceConfigurationRecord {
            schema: 1,
            revision: 1,
            instance_id: "test".to_owned(),
            domains: BTreeMap::from([(
                domain,
                DomainConfigurationRecord {
                    payment_policy: Some(PaymentPolicyRecord {
                        destination: (&destination).into(),
                        tiers: vec![PaymentTierRecord {
                            max_length: 5,
                            price_msat: 10_000,
                        }],
                    }),
                    reserved_names: vec![],
                },
            )]),
            profile: None,
            updated_at: 1,
        };
        let markup =
            configuration_markup(&configuration, "csrf", Some("example.com"), None).into_string();
        assert!(markup.contains("data-tier-row"));
        assert!(markup.contains("data-add-tier"));
        assert!(markup.contains("/admin/payment-policy/validate"));
        assert!(markup.contains("data-save-pricing"));
        assert!(markup.contains("Username shorter than"));
        assert!(markup.contains("Price (sats)"));
        assert!(markup.contains("value=\"10\""));
        assert!(!markup.contains("textarea"));
    }

    #[test]
    fn millisatoshis_are_rendered_as_exact_sat_values() {
        assert_eq!(format_sats(0), "0");
        assert_eq!(format_sats(100_000), "100");
        assert_eq!(format_sats(100_100), "100.1");
        assert_eq!(format_sats(100_001), "100.001");
    }

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
