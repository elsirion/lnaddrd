# Nostr LN Address Marketplace Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Operators publish a self-description (about/contact/terms) in their existing kind-30078 Nostr announcements; a documented JSON registration API v1 with CORS and NIP-98 auth is added; a stateless static marketplace site in `marketplace/` discovers all offers from relays and drives registration/management against any operator's API from the browser.

**Architecture:** Backend work extends existing modules (`codec.rs` config record, `ConfigurationManager`, `announcement.rs`, `admin.rs`) and adds `src/api_v1.rs` (JSON API) + `src/nostr/http_auth.rs` (NIP-98). The marketplace is no-build vanilla JS with vendored nostr-tools/Tailwind/Flowbite, mirroring `src/nostr/discovery.rs` validation rules client-side.

**Tech Stack:** Rust (axum 0.7, diesel/SQLite, nostr-sdk 0.44, maud, tower-http CORS), vanilla ES modules, nostr-tools browser bundle, qrcode-generator, Tailwind runtime + Flowbite (vendored).

**Spec:** `docs/superpowers/specs/2026-09-03-nostr-marketplace-design.md` — read it before starting any task.

## Global Constraints

- No build step for the marketplace; all assets vendored into `marketplace/assets/`.
- Marketplace stores nothing: no localStorage, no cookies; state lives only in the URL query string and memory.
- `about` ≤ 500 characters; `contact` must parse as `npub`; `terms_url` must be HTTPS.
- Capability strings: `registration-api-v1`, `nostr-auth`. API base is `<origin>/api/v1`.
- Error codes for quote/register: `invalid_input`, `unsupported_domain`, `taken`, `reserved`, `length_disabled` (400 for the first two and last one; 409 for `taken`/`reserved`).
- NIP-98: kind 27235, `created_at` within ±60 s, `u` = full request URL (public origin + path/query), `method` tag matches, `payload` = SHA-256 hex of body when a body is present, replay-guarded 120 s by event id.
- CORS (`*`, headers `content-type, authorization`) on public API routes only — never `/admin` or htmx UI routes.
- Default marketplace relays: `wss://relay.damus.io`, `wss://nos.lol`, `wss://relay.nostr.band`.
- Management token flow keeps working everywhere; Nostr auth is additive.
- After every task: `cargo fmt --all && cargo clippy --all --all-targets -- -D warnings` must pass before commit (Rust tasks).
- Commit messages end with the two trailer lines used in this repo (Co-Authored-By + Claude-Session) — see `git log`.

---

### Task 1: ServiceProfileRecord in the configuration record + persistence

**Files:**
- Modify: `src/nostr/codec.rs` (~line 179, next to `DomainConfigurationRecord`)
- Modify: `src/repository/sqlite.rs` (`service_configuration` ~line 695, `apply_configuration` helper)
- Modify: `src/nostr/discovery.rs` (test fixture ~line 178), any other `ServiceConfigurationRecord { .. }` literal the compiler flags

**Interfaces:**
- Produces: `pub struct ServiceProfileRecord { pub about: Option<String>, pub contact: Option<String>, pub terms_url: Option<String> }` with `pub fn validate(&self) -> anyhow::Result<()>`; new field `pub profile: Option<ServiceProfileRecord>` on `ServiceConfigurationRecord`; profile persisted in `service_metadata` under key `service_profile` (JSON) so it round-trips through `service_configuration`/`apply_configuration` and survives Nostr restore.

- [ ] **Step 1: Write failing tests** in the existing `#[cfg(test)] mod tests` of `src/nostr/codec.rs`:

```rust
#[test]
fn profile_validation_rules() {
    use super::ServiceProfileRecord;
    let ok = ServiceProfileRecord {
        about: Some("Community operator".to_owned()),
        contact: Some("npub1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq".to_owned()),
        terms_url: Some("https://example.com/terms".to_owned()),
    };
    // contact above is not a valid bech32 npub; build a real one from a fixed key:
    let keys = nostr_sdk::prelude::Keys::generate();
    let ok = ServiceProfileRecord {
        contact: Some(nostr_sdk::prelude::ToBech32::to_bech32(&keys.public_key()).unwrap()),
        ..ok
    };
    ok.validate().unwrap();
    assert!(ServiceProfileRecord { about: Some("x".repeat(501)), contact: None, terms_url: None }.validate().is_err());
    assert!(ServiceProfileRecord { about: None, contact: Some("not-an-npub".to_owned()), terms_url: None }.validate().is_err());
    assert!(ServiceProfileRecord { about: None, contact: None, terms_url: Some("http://insecure.example".to_owned()) }.validate().is_err());
}

#[test]
fn configuration_record_without_profile_still_decodes() {
    // Old records have no "profile" key; serde default must tolerate that.
    let json = r#"{"schema":1,"revision":1,"instance_id":"00","domains":{},"updated_at":1}"#;
    let record: ServiceConfigurationRecord = serde_json::from_str(json).unwrap();
    assert!(record.profile.is_none());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p lnaddrd profile_validation_rules configuration_record_without_profile`
Expected: compile error — `ServiceProfileRecord` not found.

- [ ] **Step 3: Implement** in `src/nostr/codec.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceProfileRecord {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub about: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contact: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terms_url: Option<String>,
}

impl ServiceProfileRecord {
    pub fn validate(&self) -> Result<()> {
        if let Some(about) = &self.about {
            ensure!(about.chars().count() <= 500, "About must be at most 500 characters");
        }
        if let Some(contact) = &self.contact {
            use nostr_sdk::prelude::FromBech32;
            nostr_sdk::prelude::PublicKey::from_bech32(contact)
                .map_err(|_| anyhow::anyhow!("Contact must be a valid npub"))?;
        }
        if let Some(terms_url) = &self.terms_url {
            let url = url::Url::parse(terms_url).context("Invalid terms URL")?;
            ensure!(url.scheme() == "https", "Terms URL must use HTTPS");
        }
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.about.is_none() && self.contact.is_none() && self.terms_url.is_none()
    }
}
```

Add to `ServiceConfigurationRecord`:

```rust
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<ServiceProfileRecord>,
```

In `BackupCodec::validate_configuration` (~line 380) add:

```rust
        if let Some(profile) = &record.profile {
            profile.validate()?;
        }
```

- [ ] **Step 4: Persist the profile.** In `src/repository/sqlite.rs`:
  - In `service_configuration` (~line 745, where the record literal is built): read the metadata key and set the field:

```rust
            let profile = service_metadata::table
                .filter(service_metadata::key.eq("service_profile"))
                .select(service_metadata::value)
                .first::<String>(&mut connection)
                .optional()?
                .map(|json| serde_json::from_str(&json))
                .transpose()?;
```

  and `profile,` in the returned `ServiceConfigurationRecord { .. }` literal.
  - Find `fn apply_configuration(` in the same file (it materializes a decoded configuration into `reserved_names` / `domain_payment_policies`). Add, inside the same transaction, an upsert/delete of the metadata key:

```rust
    match &configuration.profile {
        Some(profile) => {
            let json = serde_json::to_string(profile)?;
            diesel::replace_into(service_metadata::table)
                .values((service_metadata::key.eq("service_profile"), service_metadata::value.eq(json)))
                .execute(connection)?;
        }
        None => {
            diesel::delete(service_metadata::table.filter(service_metadata::key.eq("service_profile")))
                .execute(connection)?;
        }
    }
```

  Verify the Nostr restore path (`src/nostr/restore.rs`) materializes configuration through `apply_configuration` (grep for it); if it writes tables directly instead, mirror the same metadata write there so the profile survives restore.
- [ ] **Step 5: Fix remaining compile errors** — every other `ServiceConfigurationRecord { .. }` struct literal (e.g. `src/nostr/discovery.rs` test fixture ~line 178) gains `profile: None,`. Use `cargo check` to find them all.
- [ ] **Step 6: Run tests**

Run: `cargo test -p lnaddrd` — all pass, including the two new tests.

- [ ] **Step 7: Commit** — `feat(config): add operator profile to service configuration record`

---

### Task 2: ConfigurationManager::set_profile

**Files:**
- Modify: `src/configuration.rs`
- Test: `src/configuration.rs` (existing in-module tests)

**Interfaces:**
- Consumes: `ServiceProfileRecord` from Task 1.
- Produces: `pub async fn set_profile(&self, profile: Option<ServiceProfileRecord>) -> Result<ConfigurationUpdate>` on `ConfigurationManager`. Passing `Some(profile)` where `profile.is_empty()` or `None` clears the profile.

- [ ] **Step 1: Write failing test** in `src/configuration.rs` tests (reuse the fixture pattern from `reserved_name_becomes_active_only_through_configuration_event`, including the two `set_metadata` calls):

```rust
#[tokio::test]
async fn profile_round_trips_through_configuration() {
    // ...same repository/keys/manager setup as the existing test...
    let profile = ServiceProfileRecord {
        about: Some("Test operator".to_owned()),
        contact: None,
        terms_url: Some("https://example.com/terms".to_owned()),
    };
    let update = manager.set_profile(Some(profile.clone())).await.unwrap();
    assert!(update.active);
    assert_eq!(manager.current().await.unwrap().profile, Some(profile));
    manager.set_profile(None).await.unwrap();
    assert_eq!(manager.current().await.unwrap().profile, None);
}
```

- [ ] **Step 2: Run** `cargo test -p lnaddrd profile_round_trips` — fails: no method `set_profile`.
- [ ] **Step 3: Implement** (import `ServiceProfileRecord`; same shape as `set_payment_policy`):

```rust
    pub async fn set_profile(
        &self,
        profile: Option<ServiceProfileRecord>,
    ) -> Result<ConfigurationUpdate> {
        let profile = profile.filter(|profile| !profile.is_empty());
        if let Some(profile) = &profile {
            profile.validate()?;
        }
        let _guard = self.mutation_lock.lock().await;
        let mut configuration = self.current().await?;
        configuration.profile = profile;
        self.publish(configuration).await
    }
```

- [ ] **Step 4: Run** `cargo test -p lnaddrd profile_round_trips` — passes (the round-trip proves Task 1's `apply_configuration` write works, since `publish` → `stage_configuration` → publish → `acknowledge_event` → `apply_configuration`).
- [ ] **Step 5: Commit** — `feat(config): add set_profile to ConfigurationManager`

---

### Task 3: Announcement carries profile + new capability strings; update protocol doc 02

**Files:**
- Modify: `src/nostr/announcement.rs` (`build_event`, ~lines 135–163)
- Modify: `docs/protocol/02-service-announcements.md` (capabilities list, ~line 80)
- Test: `src/nostr/discovery.rs` tests (fixture builds a real event)

**Interfaces:**
- Consumes: `ServiceConfigurationRecord.profile` from Task 1.
- Produces: announcements whose content has `about`/`contact`/`terms_url` filled from the profile and whose `capabilities` always include `registration-api-v1` and `nostr-auth`.

- [ ] **Step 1: Write failing test** in `src/nostr/discovery.rs` tests (extend the fixture's `ServiceConfigurationRecord` with a profile):

```rust
#[test]
fn announcement_includes_profile_and_new_capabilities() {
    let (config, mut configuration, keys) = fixture();
    configuration.profile = Some(crate::nostr::codec::ServiceProfileRecord {
        about: Some("About us".to_owned()),
        contact: None,
        terms_url: Some("https://example.com/terms".to_owned()),
    });
    let event = build_event(&config, &configuration, &keys, 1_700_000_000).unwrap().unwrap();
    let announcement: ServiceAnnouncement = serde_json::from_str(&event.content).unwrap();
    assert_eq!(announcement.about.as_deref(), Some("About us"));
    assert_eq!(announcement.terms_url.as_deref(), Some("https://example.com/terms"));
    assert!(announcement.capabilities.iter().any(|c| c == "registration-api-v1"));
    assert!(announcement.capabilities.iter().any(|c| c == "nostr-auth"));
    assert!(validate_event(&event, 1_700_000_001).is_ok());
}
```

- [ ] **Step 2: Run** `cargo test -p lnaddrd announcement_includes_profile` — fails (fields are `None`, capabilities missing).
- [ ] **Step 3: Implement** in `build_event`: extend the capability set initialisation to

```rust
    let mut capabilities = BTreeSet::from([
        "management-token".to_owned(),
        "nostr-recoverable".to_owned(),
        "registration-api-v1".to_owned(),
        "nostr-auth".to_owned(),
    ]);
```

and replace the hardcoded `about: None, terms_url: None, contact: None` in the `ServiceAnnouncement` literal with:

```rust
    let profile = service_configuration.profile.clone().unwrap_or(
        crate::nostr::codec::ServiceProfileRecord { about: None, contact: None, terms_url: None },
    );
    // in the literal:
    about: profile.about,
    terms_url: profile.terms_url,
    contact: profile.contact,
```

- [ ] **Step 4: Run** `cargo test -p lnaddrd` — all pass.
- [ ] **Step 5: Update doc** `docs/protocol/02-service-announcements.md`: append to the "Allowed initial capabilities" list:

```markdown
- `registration-api-v1`: the service exposes the JSON registration API at
  `<origin>/api/v1` as defined in the registration API microstandard
  (document 03).
- `nostr-auth`: the service accepts NIP-98 HTTP authentication for address
  management as defined in document 03.
```

- [ ] **Step 6: Commit** — `feat(nostr): announce operator profile and API capabilities`

---

### Task 4: Admin "Public profile" card

**Files:**
- Modify: `src/admin.rs` (dashboard markup ~line 355; new form struct next to `PaymentPolicyForm` ~line 275; new handler next to `payment_policy_submit` ~line 797)
- Modify: `src/lib.rs` (route + import)

**Interfaces:**
- Consumes: `ConfigurationManager::set_profile` (Task 2), existing `publish_announcement`, `csrf_matches`, `label_class()`, `input_class()`, `primary_button()` helpers in `admin.rs`.
- Produces: `POST /admin/profile` handler `profile_submit`; a dashboard section prefilled from `configuration.profile`.

- [ ] **Step 1: Add form struct + handler** in `src/admin.rs`:

```rust
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
            return (StatusCode::BAD_REQUEST, Html(html! { p role="alert" class="text-sm text-red-600" { (error) } }.into_string())).into_response();
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
        format!("Profile saved; configuration revision {} is active and the announcement was republished.", update.revision)
    } else {
        format!("Profile saved; revision {} is waiting for relay acknowledgement.", update.revision)
    };
    Html(html! { p role="status" class="text-sm text-green-700" { (message) } }.into_string()).into_response()
}
```

Import `ServiceProfileRecord` in the `use crate::nostr::codec::{...}` list.

- [ ] **Step 2: Add the dashboard section** in `dashboard` markup, after the "Service public key" card:

```rust
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
```

- [ ] **Step 3: Wire the route** in `src/lib.rs`: add `profile_submit` to the `admin::{...}` import and `.route("/admin/profile", post(profile_submit))` next to the other admin routes.
- [ ] **Step 4: Verify** `cargo clippy --all --all-targets -- -D warnings && cargo test -p lnaddrd`. Manual check: `just init <relay> && just run <relay>`, open `http://localhost:8080/admin`, save a profile, confirm the success fragment appears.
- [ ] **Step 5: Commit** — `feat(admin): add public profile editor`

---

### Task 5: Structured quote outcomes in RegistrationManager

**Files:**
- Modify: `src/registration.rs`
- Test: `src/registration.rs` (add a `#[cfg(test)]` module if none exists)

**Interfaces:**
- Produces:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuoteRejection { InvalidInput, UnsupportedDomain, Taken, Reserved, LengthDisabled }
impl QuoteRejection {
    pub fn code(&self) -> &'static str;    // "invalid_input" | "unsupported_domain" | "taken" | "reserved" | "length_disabled"
    pub fn message(&self) -> &'static str; // human sentence, used by the legacy HTML flow
}
pub async fn quote_checked(&self, domain: &str, username: &str) -> Result<Result<u64, QuoteRejection>>;
```

  Outer `Err` = internal failure; inner `Err` = well-formed rejection; `Ok(0)` = free. Existing `quote()` keeps its signature, delegating to `quote_checked`.

- [ ] **Step 1: Write failing test** (in-module; repository fixture as in `src/configuration.rs` tests, plus a `FixturePublisher`):

```rust
#[tokio::test]
async fn quote_checked_reports_structured_rejections() {
    // setup: tempdir repository, metadata instance_id/configuration_revision, keys,
    // RegistrationManager::new(repo, &["example.com".to_owned()], keys, Arc::new(FixturePublisher))
    let manager = /* as above */;
    assert!(matches!(manager.quote_checked("example.com", "alice").await.unwrap(), Ok(0)));
    assert!(matches!(manager.quote_checked("other.com", "alice").await.unwrap(), Err(QuoteRejection::UnsupportedDomain)));
    assert!(matches!(manager.quote_checked("example.com", "no spaces!").await.unwrap(), Err(QuoteRejection::InvalidInput)));
}
```

- [ ] **Step 2: Run** `cargo test -p lnaddrd quote_checked_reports` — fails to compile.
- [ ] **Step 3: Implement** `QuoteRejection` (+ `code()`/`message()`), then move the body of `quote()` into `quote_checked` mapping each `ensure!` to an inner `Err(...)` instead of bailing:

```rust
    pub async fn quote_checked(&self, domain: &str, username: &str) -> Result<Result<u64, QuoteRejection>> {
        self.prune_attempts().await?;
        let Ok(domain) = domain.parse::<Domain>() else { return Ok(Err(QuoteRejection::InvalidInput)); };
        let Ok(username) = username.parse::<Username>() else { return Ok(Err(QuoteRejection::InvalidInput)); };
        if !self.domains.contains(&domain) { return Ok(Err(QuoteRejection::UnsupportedDomain)); }
        if self.repository.address_is_claimed(domain.as_str(), username.as_str()).await? { return Ok(Err(QuoteRejection::Taken)); }
        if self.repository.is_reserved(domain.as_str(), username.as_str()).await? { return Ok(Err(QuoteRejection::Reserved)); }
        let configuration = self.repository.service_configuration(&self.domains).await?;
        let Some(policy) = &configuration.domains[&domain].payment_policy else { return Ok(Ok(0)); };
        match policy_price(policy, username.as_str().len()) {
            Some(price) => Ok(Ok(price)),
            None => Ok(Err(QuoteRejection::LengthDisabled)),
        }
    }

    pub async fn quote(&self, domain: &str, username: &str) -> Result<Quote> {
        match self.quote_checked(domain, username).await? {
            Ok(0) => Ok(Quote::Free),
            Ok(price) => Ok(Quote::Paid(price)),
            Err(rejection) => bail!("{}", rejection.message()),
        }
    }
```

`message()` values must keep the existing user-facing strings: InvalidInput → "Invalid domain or username", UnsupportedDomain → "Unsupported domain", Taken → "Address is already registered or reserved", Reserved → "Reserved username", LengthDisabled → "Registration is disabled for this username length".

- [ ] **Step 4: Run** `cargo test -p lnaddrd` — all pass (existing HTML-flow behavior unchanged).
- [ ] **Step 5: Commit** — `refactor(registration): structured quote outcomes`

---

### Task 6: JSON API v1 — quote, free register, paid start/status

**Files:**
- Create: `src/api_v1.rs`
- Modify: `src/lib.rs` (module, imports, routes)
- Test: `src/api_v1.rs` in-module tests using `tower::util::ServiceExt::oneshot` (dev-dependency `tower` already present)

**Interfaces:**
- Consumes: `RegistrationManager::{quote_checked, start, status, allow_request}`, `LnaddrService::register_lnaddr`, `QuoteRejection::code()`.
- Produces routes (wired in Task 7 behind CORS):
  - `GET /api/v1/register/quote?domain=&username=` → `200 {"price_msat":u64}` | 4xx `{"error":code}`
  - `POST /api/v1/register` body `{domain, username, destination, owner_pubkey?}` → `200 {"address","management_token","active"}`
  - `POST /api/v1/register/start` same body → `200 {"id","bolt11","amount_msat","expires_at"}` (rejects free names with 400 `{"error":"free_registration"}`)
  - `GET /api/v1/register/:id` → `200 {"state","address"?,"management_token"?}` | 404 `{"error":"not_found"}`
  - Handler names: `quote_v1`, `register_v1`, `register_start_v1`, `register_status_v1`. Shared helper `fn api_error(status: StatusCode, code: &str) -> Response`.
  - `owner_pubkey` is accepted and validated (64-char lowercase hex) but only *stored* from Task 10 onward; until then handlers pass it through to signatures introduced there. In this task, accept the field and validate it, return 400 `{"error":"invalid_input"}` on bad pubkeys, and ignore it otherwise (call the current 3-arg service methods).

- [ ] **Step 1: Write failing tests** (in `src/api_v1.rs` `#[cfg(test)]`): build a minimal `Router` exactly like `normal_router` does but with a tempdir repository, `FixturePublisher`-style publisher, and domain `example.com` (extract a small `test_router()` helper; copy the AppState wiring from `normal_router`, replacing `NostrPublisher::connect` with a fixture publisher — add `#[cfg(test)] pub fn test_app_state(...)` in `lib.rs` if that is simpler). Tests:

```rust
#[tokio::test]
async fn quote_returns_price_and_structured_errors() {
    let app = test_router().await;
    let response = app.clone().oneshot(Request::get("/api/v1/register/quote?domain=example.com&username=alice").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value = read_json(response).await;
    assert_eq!(body["price_msat"], 0);
    let response = app.oneshot(Request::get("/api/v1/register/quote?domain=nope.com&username=alice").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body: serde_json::Value = read_json(response).await;
    assert_eq!(body["error"], "unsupported_domain");
}

#[tokio::test]
async fn free_registration_returns_token() { /* POST /api/v1/register with a destination the test validator accepts; assert address + management_token fields */ }

#[tokio::test]
async fn unknown_attempt_is_not_found() { /* GET /api/v1/register/does-not-exist → 404 {"error":"not_found"} */ }
```

For `free_registration_returns_token`, destination validation performs network I/O via `PaymentClient`; construct `DirectLnaddrService` with a no-op `DestinationValidator` if the existing tests in `src/service/direct.rs` already do so (they do — copy that fixture pattern).

- [ ] **Step 2: Run** `cargo test -p lnaddrd api_v1` — fails: module doesn't exist.
- [ ] **Step 3: Implement `src/api_v1.rs`:**

```rust
use axum::{Json, extract::{ConnectInfo, Path, Query, State}, http::StatusCode, response::{IntoResponse, Response}};
use serde::Deserialize;
use serde_json::json;
use std::net::SocketAddr;

use crate::{AppState, registration::{QuoteRejection, RegistrationStatus}};

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
pub struct QuoteQuery { pub domain: String, pub username: String }

pub async fn quote_v1(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Query(query): Query<QuoteQuery>,
) -> Response {
    if !state.registration_manager.allow_request(peer.ip(), "quote", 30).await {
        return api_error(StatusCode::TOO_MANY_REQUESTS, "rate_limited");
    }
    match state.registration_manager.quote_checked(&query.domain, &query.username).await {
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
        let valid = pubkey.len() == 64 && pubkey.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase());
        if !valid { return Err(api_error(StatusCode::BAD_REQUEST, "invalid_input")); }
    }
    Ok(())
}

pub async fn register_v1(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(body): Json<RegisterBody>,
) -> Response {
    if !state.registration_manager.allow_request(peer.ip(), "start", 10).await {
        return api_error(StatusCode::TOO_MANY_REQUESTS, "rate_limited");
    }
    if let Err(response) = validate_owner_pubkey(&body.owner_pubkey) { return response; }
    match state.registration_manager.quote_checked(&body.domain, &body.username).await {
        Ok(Ok(0)) => {}
        Ok(Ok(_)) => return api_error(StatusCode::BAD_REQUEST, "payment_required"),
        Ok(Err(rejection)) => return rejection_response(rejection),
        Err(_) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, "internal"),
    }
    match state.service.register_lnaddr(&body.domain, &body.username, &body.destination).await {
        Ok(response) => Json(json!({
            "address": response.lnaddr,
            "management_token": response.authentication_token,
            "active": response.active,
        })).into_response(),
        Err(_) => api_error(StatusCode::BAD_REQUEST, "invalid_input"),
    }
}

pub async fn register_start_v1(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(body): Json<RegisterBody>,
) -> Response {
    if !state.registration_manager.allow_request(peer.ip(), "start", 10).await {
        return api_error(StatusCode::TOO_MANY_REQUESTS, "rate_limited");
    }
    if let Err(response) = validate_owner_pubkey(&body.owner_pubkey) { return response; }
    match state.registration_manager.quote_checked(&body.domain, &body.username).await {
        Ok(Ok(0)) => return api_error(StatusCode::BAD_REQUEST, "free_registration"),
        Ok(Ok(_)) => {}
        Ok(Err(rejection)) => return rejection_response(rejection),
        Err(_) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, "internal"),
    }
    match state.registration_manager.start(&body.domain, &body.username, &body.destination).await {
        Ok(started) => Json(json!({
            "id": started.id,
            "bolt11": started.invoice,
            "amount_msat": started.amount_msat,
            "expires_at": started.expires_at,
        })).into_response(),
        Err(_) => api_error(StatusCode::BAD_REQUEST, "invalid_input"),
    }
}

pub async fn register_status_v1(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.registration_manager.status(&id).await {
        Ok(RegistrationStatus::Pending) => Json(json!({ "state": "pending_payment" })).into_response(),
        Ok(RegistrationStatus::Publishing) => Json(json!({ "state": "publishing" })).into_response(),
        Ok(RegistrationStatus::Expired) => Json(json!({ "state": "expired" })).into_response(),
        Ok(RegistrationStatus::Complete { address, management_token }) => Json(json!({
            "state": "complete", "address": address, "management_token": management_token,
        })).into_response(),
        Err(_) => api_error(StatusCode::NOT_FOUND, "not_found"),
    }
}
```

- [ ] **Step 4: Wire routes** in `src/lib.rs` (`pub mod api_v1;` + imports):

```rust
        .route("/api/v1/register/quote", get(quote_v1))
        .route("/api/v1/register", post(register_v1))
        .route("/api/v1/register/start", post(register_start_v1))
        .route("/api/v1/register/:id", get(register_status_v1))
```

- [ ] **Step 5: Run** `cargo test -p lnaddrd` — all pass.
- [ ] **Step 6: Commit** — `feat(api): add JSON registration API v1`

---

### Task 7: CORS on public routes

**Files:**
- Modify: `Cargo.toml` (add `tower-http = { version = "0.6", features = ["cors"] }`)
- Modify: `src/lib.rs` (`normal_router`)
- Test: `src/api_v1.rs` tests

**Interfaces:**
- Produces: a `fn cors_layer() -> tower_http::cors::CorsLayer` in `lib.rs`; public routes (everything under `/api/v1`, `/domains`, `/lnaddress/*`, `/.well-known/lnaddrd.json`, `/.well-known/lnurlp/:username`, `/lnurl/:username`) respond with `access-control-allow-origin: *` and answer preflight; `/admin*` and UI routes do not.

- [ ] **Step 1: Write failing tests** in `src/api_v1.rs`:

```rust
#[tokio::test]
async fn cors_allows_public_api_and_not_admin() {
    let app = test_router().await;
    let response = app.clone().oneshot(
        Request::get("/api/v1/register/quote?domain=example.com&username=alice")
            .header("origin", "https://market.example").body(Body::empty()).unwrap()
    ).await.unwrap();
    assert_eq!(response.headers().get("access-control-allow-origin").unwrap(), "*");
    let preflight = app.clone().oneshot(
        Request::builder().method("OPTIONS").uri("/api/v1/register")
            .header("origin", "https://market.example")
            .header("access-control-request-method", "POST")
            .header("access-control-request-headers", "content-type, authorization")
            .body(Body::empty()).unwrap()
    ).await.unwrap();
    assert_eq!(preflight.status(), StatusCode::OK);
    let admin = app.oneshot(Request::get("/admin/login").header("origin", "https://market.example").body(Body::empty()).unwrap()).await.unwrap();
    assert!(admin.headers().get("access-control-allow-origin").is_none());
}
```

- [ ] **Step 2: Run** — fails (no CORS headers).
- [ ] **Step 3: Implement**: in `normal_router`, split routes into two `Router`s with the same state — `public` (the routes listed in Interfaces plus the API v1 routes) and `private` (admin, UI/htmx, assets, health) — apply the layer to `public` only, then `public.merge(private)`:

```rust
use tower_http::cors::{Any, CorsLayer};

fn cors_layer() -> CorsLayer {
    CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([Method::GET, Method::POST, Method::PUT, Method::DELETE, Method::OPTIONS])
        .allow_headers([axum::http::header::CONTENT_TYPE, axum::http::header::AUTHORIZATION])
}
```

- [ ] **Step 4: Run** `cargo test -p lnaddrd` — all pass. `cargo clippy --all --all-targets -- -D warnings`.
- [ ] **Step 5: Commit** — `feat(api): enable CORS on public endpoints`

---

### Task 8: NIP-98 verification module

**Files:**
- Create: `src/nostr/http_auth.rs`; register in `src/nostr/mod.rs`
- Modify: `Cargo.toml` (add `base64 = "0.22"`)

**Interfaces:**
- Produces:

```rust
pub struct Nip98Auth { pub pubkey: String /* 64-char lowercase hex */, pub event_id: String }
pub fn verify_nip98(authorization: &str, method: &str, url: &str, body: Option<&[u8]>, now: u64) -> anyhow::Result<Nip98Auth>;
pub struct Nip98ReplayGuard { /* Mutex<HashMap<String, u64>> */ }
impl Nip98ReplayGuard {
    pub fn new() -> Self;
    pub fn check_and_insert(&self, event_id: &str, now: u64) -> bool; // false = replay; prunes entries older than 120 s
}
```

- [ ] **Step 1: Write failing tests** (in-module). Build events with `nostr_sdk::prelude::{Keys, EventBuilder, Kind, Tag, Timestamp}`; helper:

```rust
fn auth_header(keys: &Keys, url: &str, method: &str, payload: Option<&[u8]>, created_at: u64) -> String {
    let mut tags = vec![Tag::parse(["u", url]).unwrap(), Tag::parse(["method", method]).unwrap()];
    if let Some(payload) = payload {
        use sha2::{Digest, Sha256};
        tags.push(Tag::parse(["payload", &hex::encode(Sha256::digest(payload))]).unwrap());
    }
    let event = EventBuilder::new(Kind::HttpAuth, "").tags(tags)
        .custom_created_at(Timestamp::from_secs(created_at))
        .sign_with_keys(keys).unwrap();
    use base64::Engine;
    format!("Nostr {}", base64::engine::general_purpose::STANDARD.encode(serde_json::to_string(&event).unwrap()))
}

#[test]
fn accepts_valid_header_and_rejects_tampering() {
    let keys = Keys::generate();
    let url = "https://pay.example.com/api/v1/addresses";
    let header = auth_header(&keys, url, "GET", None, 1_700_000_000);
    let auth = verify_nip98(&header, "GET", url, None, 1_700_000_010).unwrap();
    assert_eq!(auth.pubkey, keys.public_key().to_string());
    assert!(verify_nip98(&header, "POST", url, None, 1_700_000_010).is_err());          // wrong method
    assert!(verify_nip98(&header, "GET", "https://other.example/x", None, 1_700_000_010).is_err()); // wrong url
    assert!(verify_nip98(&header, "GET", url, None, 1_700_009_999).is_err());           // stale (> 60 s)
    let with_body = auth_header(&keys, url, "POST", Some(b"{}"), 1_700_000_000);
    assert!(verify_nip98(&with_body, "POST", url, Some(b"{}"), 1_700_000_010).is_ok());
    assert!(verify_nip98(&with_body, "POST", url, Some(b"{-}"), 1_700_000_010).is_err()); // body mismatch
}

#[test]
fn replay_guard_blocks_second_use() {
    let guard = Nip98ReplayGuard::new();
    assert!(guard.check_and_insert("id1", 1000));
    assert!(!guard.check_and_insert("id1", 1010));
    assert!(guard.check_and_insert("id1", 1200)); // pruned after 120 s
}
```

- [ ] **Step 2: Run** `cargo test -p lnaddrd http_auth` — fails to compile.
- [ ] **Step 3: Implement** `verify_nip98`: strip `"Nostr "` prefix (case-insensitive), base64-decode, `Event::from_json`, `event.verify()?`, `ensure!(event.kind == Kind::HttpAuth)`, `ensure!(event.created_at.as_secs().abs_diff(now) <= 60)`, extract `u`/`method`/`payload` tags via the `tag.as_slice()` pattern used in `discovery.rs`, `ensure!` `u == url` and `method` equals `method` case-insensitively; if `body` is `Some`, require the `payload` tag to equal `hex::encode(Sha256::digest(body))`; if `body` is `None`, require no payload tag. Return `Nip98Auth { pubkey: event.pubkey.to_string(), event_id: event.id.to_string() }`. Implement `Nip98ReplayGuard` with `std::sync::Mutex<HashMap<String, u64>>`, pruning entries where `now - inserted > 120` on every call.
- [ ] **Step 4: Run** `cargo test -p lnaddrd http_auth` — passes.
- [ ] **Step 5: Commit** — `feat(nostr): add NIP-98 HTTP auth verification`

---

### Task 9: owner_pubkey storage — migration, repository, backup codec

**Files:**
- Create: `migrations/10_owner_pubkey/up.sql`, `migrations/10_owner_pubkey/down.sql`
- Modify: `src/repository/sqlite.rs` (both `diesel::table!` blocks, `PaymentAddressEntry` ~line 1500, `RegistrationAttempt` ~line 1227, `ManagedAddress` ~line 1184, `stage_payment_address` ~line 50, `get_address_for_management`, restore plumbing `RestoredAddress` ~line 1165 / `restore_records` ~line 1086)
- Modify: `src/nostr/codec.rs` (`AddressRecord`, `UpdatedBy`)

**Interfaces:**
- Produces:
  - SQL: `payment_addresses.owner_pubkey TEXT NULL` (+ partial index), `registration_attempts.owner_pubkey TEXT NULL`.
  - `stage_payment_address(..., registration_attempt_id: Option<&str>, owner_pubkey: Option<&str>)` — new trailing parameter.
  - `ManagedAddress.owner_pubkey: Option<String>`.
  - `pub async fn addresses_for_owner(&self, owner_pubkey: &str) -> Result<Vec<OwnedAddress>>` with `pub struct OwnedAddress { pub domain: String, pub username: String, pub destination: String }` (state = `active` only).
  - `AddressRecord.owner_pubkey: Option<String>` (`#[serde(default, skip_serializing_if = "Option::is_none")]`) + builder `pub fn with_owner(mut self, owner_pubkey: Option<String>) -> Self`; `UpdatedBy::Owner` variant.
  - `RegistrationAttempt.owner_pubkey: Option<String>`.

- [ ] **Step 1: Write the migration**

`migrations/10_owner_pubkey/up.sql`:
```sql
ALTER TABLE payment_addresses ADD COLUMN owner_pubkey TEXT;
CREATE INDEX payment_addresses_owner ON payment_addresses (owner_pubkey) WHERE owner_pubkey IS NOT NULL;
ALTER TABLE registration_attempts ADD COLUMN owner_pubkey TEXT;
```
`migrations/10_owner_pubkey/down.sql`:
```sql
DROP INDEX payment_addresses_owner;
ALTER TABLE payment_addresses DROP COLUMN owner_pubkey;
ALTER TABLE registration_attempts DROP COLUMN owner_pubkey;
```

- [ ] **Step 2: Write failing test** in `src/repository/sqlite.rs` tests:

```rust
#[tokio::test]
async fn owner_pubkey_round_trips_and_lists() {
    // tempdir repository; stage a payment address with owner_pubkey Some("aa..64"),
    // acknowledge its event, then:
    let managed = repo.get_address_for_management("example.com", "alice").await.unwrap().unwrap();
    assert_eq!(managed.owner_pubkey.as_deref(), Some(OWNER));
    let owned = repo.addresses_for_owner(OWNER).await.unwrap();
    assert_eq!(owned.len(), 1);
    assert_eq!(owned[0].username, "alice");
    assert!(repo.addresses_for_owner(&"b".repeat(64)).await.unwrap().is_empty());
}
```

(Reuse whatever existing sqlite tests do to construct a stageable event — grep for `stage_payment_address` in existing tests and copy the fixture.)

- [ ] **Step 3: Implement**: add `owner_pubkey -> Nullable<Text>` to both `table!` blocks (column order must match the DB: appended last), add fields to `PaymentAddressEntry` (as `owner_pubkey: Option<String>`, not underscore-prefixed), `RegistrationAttempt`, `ManagedAddress` (populate in the `TryInto` conversion), thread the new parameter through `stage_payment_address` into the insert, implement `addresses_for_owner` filtering `owner_pubkey.eq(value)` and `state.eq("active")`. Update all existing `stage_payment_address` call sites (`src/service/direct.rs`, `src/registration.rs`, `src/nostr/restore.rs` if present, tests) to pass `None` for now. In `codec.rs` add the `owner_pubkey` field + `with_owner` builder + `UpdatedBy::Owner`, and set `owner_pubkey: None` in `active()`/`tombstone()` constructors. Thread restore: `RestoredAddress` gains `owner_pubkey: Option<String>` sourced from the decoded `AddressRecord`, and `restore_records` writes the column (follow how `authentication_token` hash flows through restore today).
- [ ] **Step 4: Run** `cargo test -p lnaddrd` — all pass.
- [ ] **Step 5: Commit** — `feat(storage): persist address owner pubkey`

---

### Task 10: Owner plumbing through service + registration manager

**Files:**
- Modify: `src/service/mod.rs` (trait + `ManagementAuth`), `src/service/direct.rs`, `src/registration.rs`, call sites in `src/api.rs`, `src/ui.rs`

**Interfaces:**
- Produces:

```rust
// src/service/mod.rs
#[derive(Debug, Clone)]
pub enum ManagementAuth { Token(String), Owner(String) } // Owner = verified 64-char hex pubkey

// trait ILnaddrService — changed signatures:
async fn register_lnaddr(&self, domain: &str, username: &str, destination: &str, owner_pubkey: Option<&str>) -> Result<RegisterResponse>;
async fn remove_lnaddr(&self, domain: &str, username: &str, auth: &ManagementAuth) -> Result<()>;
async fn update_lnaddr(&self, domain: &str, username: &str, destination: &str, auth: &ManagementAuth) -> Result<bool>;

// src/registration.rs — changed signature:
pub async fn start(&self, domain: &str, username: &str, destination: &str, owner_pubkey: Option<&str>) -> Result<StartedRegistration>;
```

- [ ] **Step 1: Write failing test** in `src/service/direct.rs` tests (copy the existing register/remove test fixture):

```rust
#[tokio::test]
async fn owner_auth_manages_address_and_stranger_cannot() {
    let owner = "a".repeat(64);
    let response = service.register_lnaddr("example.com", "alice", VALID_LNURL, Some(&owner)).await.unwrap();
    assert!(service.update_lnaddr("example.com", "alice", VALID_LNURL, &ManagementAuth::Owner("b".repeat(64))).await.is_err());
    assert!(service.update_lnaddr("example.com", "alice", VALID_LNURL, &ManagementAuth::Owner(owner.clone())).await.is_ok());
    // token fallback still works:
    service.remove_lnaddr("example.com", "alice", &ManagementAuth::Token(response.authentication_token)).await.unwrap();
}
```

- [ ] **Step 2: Run** — fails to compile.
- [ ] **Step 3: Implement** in `direct.rs`: add a private helper

```rust
fn verify_management_auth(managed: &ManagedAddress, auth: &ManagementAuth) -> Result<UpdatedBy> {
    match auth {
        ManagementAuth::Token(token) => {
            let parsed_hash = PasswordHash::new(&managed.authentication_token_hash)
                .map_err(|_| anyhow::anyhow!("Invalid management token"))?;
            Argon2::default().verify_password(token.as_bytes(), &parsed_hash)
                .map_err(|_| anyhow::anyhow!("Invalid management token"))?;
            Ok(UpdatedBy::Token)
        }
        ManagementAuth::Owner(pubkey) => {
            ensure!(managed.owner_pubkey.as_deref() == Some(pubkey.as_str()), "Not the address owner");
            Ok(UpdatedBy::Owner)
        }
    }
}
```

Use it in `remove_lnaddr`/`update_lnaddr` (replacing the inline argon2 blocks; the returned `UpdatedBy` goes into the tombstone/updated record). In `register_lnaddr`, accept `owner_pubkey`, call `.with_owner(owner_pubkey.map(str::to_owned))` on the record, and pass it to `stage_payment_address`. In `update_lnaddr`, preserve the existing owner: `.with_owner(managed.owner_pubkey.clone())`.
In `registration.rs`: `start` stores `owner_pubkey` on the attempt; `status` applies `.with_owner(attempt.owner_pubkey.clone())` and passes it to `stage_payment_address`.
Update call sites: `src/api.rs` `register_lnaddr_handler` passes `None`, `remove_lnaddr_handler`/`update_lnaddr_handler` wrap the body token in `ManagementAuth::Token`; `src/ui.rs` passes `None` for owner and wraps tokens likewise; `registration_start` in `ui.rs` passes `None`; `src/api_v1.rs` `register_v1` passes `body.owner_pubkey.as_deref()` to `register_lnaddr` and `register_start_v1` passes it to `start` (NIP-98-derived owners arrive in Task 11).
- [ ] **Step 4: Run** `cargo test -p lnaddrd` — all pass.
- [ ] **Step 5: Commit** — `feat(service): nostr owner authentication for address management`

---

### Task 11: Wire NIP-98 into the HTTP layer + /api/v1/addresses

**Files:**
- Modify: `src/api_v1.rs`, `src/api.rs` (update/remove handlers), `src/lib.rs` (AppState + route)

**Interfaces:**
- Consumes: `verify_nip98`, `Nip98ReplayGuard` (Task 8), `addresses_for_owner` (Task 9), `ManagementAuth` (Task 10).
- Produces:
  - `AppState.nip98_guard: Arc<Nip98ReplayGuard>`.
  - Helper in `api_v1.rs`: `pub fn nip98_from_request(state: &AppState, headers: &HeaderMap, method: &str, uri: &OriginalUri, body: Option<&[u8]>) -> Result<Option<String>, Response>` — `Ok(None)` if no `Authorization` header; `Ok(Some(pubkey))` on valid auth (URL = normalized public origin + `uri.path_and_query()`; replay-guarded); `Err(401 {"error":"unauthorized"})` on invalid auth or when `public_base_url` is unset.
  - `GET /api/v1/addresses` → `{"addresses":[{"domain","username","destination"}]}`, 401 without valid NIP-98.
  - `POST /api/v1/register` and `/api/v1/register/start` accept NIP-98: signer pubkey becomes the owner; if body `owner_pubkey` is also present they must match (400 `owner_mismatch`). These two handlers switch to `(headers, OriginalUri, bytes: axum::body::Bytes)` extraction and `serde_json::from_slice` so the raw body is available for the payload hash.
  - `/lnaddress/update` and `/lnaddress/remove` in `src/api.rs`: same raw-body pattern; if a valid NIP-98 header is present use `ManagementAuth::Owner`, else fall back to the body token (`authentication_token` becomes `Option<String>` in those request structs; missing both → 401).

- [ ] **Step 1: Write failing tests** in `src/api_v1.rs` (reuse `auth_header` from Task 8's test module by making it `#[cfg(test)] pub`):

```rust
#[tokio::test]
async fn addresses_requires_and_honors_nip98() {
    // register alice with owner = keys.public_key() via POST /api/v1/register with NIP-98 header,
    // then GET /api/v1/addresses with a fresh NIP-98 header → 200 with one entry;
    // GET without header → 401; GET with header signed by different keys → 200 with empty list.
}

#[tokio::test]
async fn owner_mismatch_is_rejected() {
    // POST /api/v1/register with NIP-98 signed by keys A but body owner_pubkey of keys B → 400 owner_mismatch
}
```

The test router's `Config.public_base_url` must be `Some("https://example.com".to_owned())` and signed `u` tags use `https://example.com<path>`.

- [ ] **Step 2: Run** — fails.
- [ ] **Step 3: Implement** the helper and handlers. URL construction: `let origin = crate::nostr::announcement::normalized_origin(state.config.public_base_url.as_deref().ok_or_else(unauthorized)?)`; full URL = `format!("{origin}{}", uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("/"))`. Use `state.nip98_guard.check_and_insert(&auth.event_id, now)`; on false return 401. Add the `addresses_v1` handler:

```rust
pub async fn addresses_v1(State(state): State<AppState>, headers: HeaderMap, uri: OriginalUri) -> Response {
    match nip98_from_request(&state, &headers, "GET", &uri, None) {
        Ok(Some(pubkey)) => match state.repository.addresses_for_owner(&pubkey).await {
            Ok(addresses) => Json(json!({ "addresses": addresses.iter().map(|a| json!({
                "domain": a.domain, "username": a.username, "destination": a.destination })).collect::<Vec<_>>() })).into_response(),
            Err(_) => api_error(StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        },
        Ok(None) => api_error(StatusCode::UNAUTHORIZED, "unauthorized"),
        Err(response) => response,
    }
}
```

Route: `.route("/api/v1/addresses", get(addresses_v1))` in the public (CORS) router. Add `nip98_guard: Arc::new(Nip98ReplayGuard::new())` to `AppState` construction.
- [ ] **Step 4: Run** `cargo test -p lnaddrd` — all pass. `cargo clippy --all --all-targets -- -D warnings`.
- [ ] **Step 5: Commit** — `feat(api): NIP-98 authentication and owned-address listing`

---

### Task 12: Protocol doc 03 + README API section

**Files:**
- Create: `docs/protocol/03-registration-api.md`
- Modify: `README.md` (after the "Service discovery" section)

**Interfaces:** none (documentation). Content must match Tasks 6–11 exactly: endpoint paths, request/response JSON, error codes and statuses, NIP-98 rules (±60 s, `u`/`method`/`payload` tags, replay guard, public-origin URL), the `registration-api-v1` / `nostr-auth` capabilities, and the note that the management token remains the fallback. Follow the RFC-ish style of `docs/protocol/02-service-announcements.md` (Abstract, endpoint sections with JSON examples, Security and privacy considerations, Standards composed listing NIP-01/NIP-98). Include one full example per endpoint copied from the implemented handlers' shapes.

- [ ] **Step 1: Write the document.** Sections: Abstract; Discovery (capability strings + API base `<origin>/api/v1`); Quote; Free registration; Paid registration (start + poll, LUD-21 note, token-shown-once semantics); Authentication (NIP-98 profile as implemented); Owned addresses; Management (legacy `/lnaddress/update`/`remove` with token-or-NIP-98); Errors table; Security and privacy considerations; Standards composed.
- [ ] **Step 2: Cross-check** every path/field/status against `src/api_v1.rs` and `src/api.rs` — no drift.
- [ ] **Step 3: README**: add an "HTTP API" subsection pointing to doc 03 and listing the endpoints one line each.
- [ ] **Step 4: Commit** — `docs: specify registration API v1`

---

### Task 13: Marketplace scaffold — vendored assets, shell page, justfile

**Files:**
- Create: `marketplace/index.html`, `marketplace/js/config.js`, `marketplace/assets/` (5 vendored files)
- Modify: `justfile`

**Interfaces:**
- Produces: `marketplace/` opens in a browser via `just marketplace-serve` (port 8081); `window.NostrTools` and `qrcode` globals available; `DEFAULT_RELAYS` exported from `js/config.js`; page shell has `#relay-editor`, `#relay-status`, `#operators` (Browse tab), `#manage` (Manage tab), `#modal-root` containers that later tasks fill.

- [ ] **Step 1: Vendor assets**

```bash
cp assets/tailwindcss-3.4.17.js assets/flowbite-1.7.0.min.css assets/flowbite-1.7.0.min.js marketplace/assets/
curl -fL https://cdn.jsdelivr.net/npm/nostr-tools@2.10.4/lib/nostr.bundle.js -o marketplace/assets/nostr-tools-2.10.4.bundle.js
curl -fL https://cdn.jsdelivr.net/npm/qrcode-generator@1.4.4/qrcode.min.js -o marketplace/assets/qrcode-generator-1.4.4.min.js
```

Sanity-check the nostr-tools bundle: `grep -c "SimplePool" marketplace/assets/nostr-tools-2.10.4.bundle.js` ≥ 1. If jsDelivr's path 404s, use `https://unpkg.com/nostr-tools@2.10.4/lib/nostr.bundle.js`.

- [ ] **Step 2: Write `marketplace/js/config.js`:**

```js
export const DEFAULT_RELAYS = [
  "wss://relay.damus.io",
  "wss://nos.lol",
  "wss://relay.nostr.band",
];
export const ANNOUNCEMENT_KIND = 30078;
export const ANNOUNCEMENT_TAG = "lightning-address-service";
export const ANNOUNCEMENT_PREFIX = "lnaddrd:service:v1:";
```

- [ ] **Step 3: Write `marketplace/index.html`** — same design language as `src/ui.rs`/`src/admin.rs` (`bg-gray-50` body, white `rounded-lg` shadow cards, `bg-blue-700` buttons):

```html
<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>LN Address Marketplace</title>
  <link rel="stylesheet" href="assets/flowbite-1.7.0.min.css">
  <script src="assets/tailwindcss-3.4.17.js"></script>
  <script src="assets/flowbite-1.7.0.min.js"></script>
  <script src="assets/nostr-tools-2.10.4.bundle.js"></script>
  <script src="assets/qrcode-generator-1.4.4.min.js"></script>
</head>
<body class="bg-gray-50 min-h-screen text-gray-900">
  <main class="mx-auto max-w-5xl p-4 sm:p-6 lg:p-8 space-y-6">
    <header class="flex flex-col gap-2 sm:flex-row sm:items-end sm:justify-between">
      <div>
        <h1 class="text-3xl font-bold tracking-tight">LN Address Marketplace</h1>
        <p class="mt-1 text-sm text-gray-500">Operators discovered over Nostr. Announcements are claims, not endorsements.</p>
      </div>
      <button id="connect-nostr" class="text-white bg-blue-700 hover:bg-blue-800 focus:ring-4 focus:ring-blue-300 font-medium rounded-lg text-sm px-4 py-2">Connect Nostr</button>
    </header>
    <section id="relay-section" class="rounded-lg border border-gray-200 bg-white p-4 shadow-sm">
      <h2 class="text-sm font-semibold text-gray-700">Discovery relays</h2>
      <div id="relay-editor" class="mt-2 flex flex-wrap items-center gap-2"></div>
      <div id="relay-status" class="mt-2 flex flex-wrap gap-2 text-xs"></div>
    </section>
    <nav class="flex gap-4 border-b border-gray-200 text-sm font-medium">
      <button data-tab="browse" class="tab-btn border-b-2 border-blue-700 px-1 pb-2 text-blue-700">Browse</button>
      <button data-tab="manage" class="tab-btn border-b-2 border-transparent px-1 pb-2 text-gray-500 hover:text-gray-700">Manage</button>
    </nav>
    <section id="operators" class="grid gap-4 sm:grid-cols-2"></section>
    <section id="manage" class="hidden space-y-4"></section>
  </main>
  <div id="modal-root"></div>
  <script type="module" src="js/app.js"></script>
</body>
</html>
```

Create a placeholder `marketplace/js/app.js` for now: tab switching only (click handler toggling `hidden` on `#operators`/`#manage` and the border/text classes on `.tab-btn`).

- [ ] **Step 4: justfile recipe:**

```make
# Serve the static marketplace on http://localhost:8081
marketplace-serve:
    python3 -m http.server 8081 -d marketplace
```

- [ ] **Step 5: Verify** `just marketplace-serve`, open http://localhost:8081 — shell renders, tabs switch, no console errors (check with browser or `curl -s localhost:8081 | grep -c tab-btn` = 2).
- [ ] **Step 6: Commit** — `feat(marketplace): scaffold static site with vendored assets`

---

### Task 14: Pure announcement validation module + node tests

**Files:**
- Create: `marketplace/js/announcement.js`, `marketplace/test/announcement.test.mjs`
- Modify: `justfile` (test recipe)

**Interfaces:**
- Consumes: constants from `js/config.js`.
- Produces (pure functions, no DOM, no NostrTools — signature checks happen in `app.js`):

```js
// Returns {ok: true, origin, dtag, announcement} or {ok: false, error: "..."}.
// Mirrors src/nostr/discovery.rs::validate_event minus the signature check.
export function validateAnnouncement(event, nowSecs)
// "free" | "from 1 sat" | "from 1,000 sats" | null (no pricing entry for domain)
export function priceSummary(announcement, domain)
// Deduplicate: keep newest per pubkey+dtag coordinate (tie: larger id)
export function upsertByCoordinate(map, validated, event)
```

- [ ] **Step 1: Write failing tests** `marketplace/test/announcement.test.mjs`:

```js
import test from "node:test";
import assert from "node:assert/strict";
import { validateAnnouncement, priceSummary, upsertByCoordinate } from "../js/announcement.js";

const ORIGIN = "https://pay.example.com";
function makeEvent(overrides = {}, content = {}) {
  const announcement = {
    schema: 1, origin: ORIGIN, domains: ["pay.example.com"],
    registration_url: `${ORIGIN}/`, capabilities: ["registration-api-v1"],
    pricing: [{ domain: "pay.example.com", currency: "msat",
      tiers: [{ max_length: 4, price: 1000000 }, { max_length: 64, price: 0 }] }],
    ...content,
  };
  return {
    kind: 30078, pubkey: "a".repeat(64), id: "e".repeat(64), created_at: 1000,
    tags: [["d", `lnaddrd:service:v1:${ORIGIN}`], ["t", "lightning-address-service"], ["expiration", "2000"]],
    content: JSON.stringify(announcement), ...overrides,
  };
}

test("valid announcement passes", () => {
  const result = validateAnnouncement(makeEvent(), 1500);
  assert.equal(result.ok, true);
  assert.equal(result.origin, ORIGIN);
});
test("expired announcement fails", () => {
  assert.equal(validateAnnouncement(makeEvent(), 2001).ok, false);
});
test("origin mismatch fails", () => {
  assert.equal(validateAnnouncement(makeEvent({}, { origin: "https://evil.example" }), 1500).ok, false);
});
test("retired service fails", () => {
  assert.equal(validateAnnouncement(makeEvent({}, { status: "retired" }), 1500).ok, false);
});
test("unsorted domains fail", () => {
  assert.equal(validateAnnouncement(makeEvent({}, { domains: ["b.com", "a.com"] }), 1500).ok, false);
});
test("registration url on other origin fails", () => {
  assert.equal(validateAnnouncement(makeEvent({}, { registration_url: "https://other.example/" }), 1500).ok, false);
});
test("price summary", () => {
  const { announcement } = validateAnnouncement(makeEvent(), 1500);
  assert.equal(priceSummary(announcement, "pay.example.com"), "free");
  const paid = validateAnnouncement(makeEvent({}, { pricing: [{ domain: "pay.example.com", currency: "msat", tiers: [{ max_length: 64, price: 2000 }] }] }), 1500);
  assert.equal(priceSummary(paid.announcement, "pay.example.com"), "from 2 sats");
  assert.equal(priceSummary(announcement, "unknown.example"), null);
});
test("upsert keeps newest per coordinate", () => {
  const map = new Map();
  const older = makeEvent({ created_at: 1000, id: "1".repeat(64) });
  const newer = makeEvent({ created_at: 1100, id: "2".repeat(64) });
  upsertByCoordinate(map, validateAnnouncement(older, 1500), older);
  upsertByCoordinate(map, validateAnnouncement(newer, 1500), newer);
  upsertByCoordinate(map, validateAnnouncement(older, 1500), older);
  assert.equal(map.size, 1);
  assert.equal([...map.values()][0].event.created_at, 1100);
});
```

- [ ] **Step 2: Run** `node --test marketplace/test/` — fails (module missing).
- [ ] **Step 3: Implement `marketplace/js/announcement.js`** — checks in order, each returning `{ok:false, error}` on failure: kind is 30078; `d` tag exists and starts with `ANNOUNCEMENT_PREFIX`; the origin suffix parses via `new URL(origin)` with `url.origin === origin` and protocol `https:`; `t` tag `lightning-address-service` present; every `expiration` tag value > `nowSecs`; content parses as JSON with `schema === 1`; `status !== "retired"`; `announcement.origin === origin`; `domains` is a non-empty array, sorted and unique; `new URL(registration_url).origin === origin`. `priceSummary`: find pricing entry for domain; no tiers → null; min price 0 → `"free"`; else `` `from ${Math.ceil(min/1000).toLocaleString("en-US")} sat(s)` `` (singular for 1). `upsertByCoordinate(map, validated, event)`: no-op unless `validated.ok`; key `` `${event.pubkey}:${validated.dtag}` ``; replace when `(created_at, id)` is greater; stored value `{validated, event}`.
- [ ] **Step 4: Run** `node --test marketplace/test/` — all pass. Add justfile recipe:

```make
# Run marketplace JS tests (requires node >= 20)
marketplace-test:
    node --test marketplace/test/
```

- [ ] **Step 5: Commit** — `feat(marketplace): announcement validation module`

---

### Task 15: Discovery, operator cards, domain verification

**Files:**
- Create: `marketplace/js/relays.js`, `marketplace/js/render.js`
- Modify: `marketplace/js/app.js`

**Interfaces:**
- Consumes: `validateAnnouncement`, `upsertByCoordinate`, `priceSummary` (Task 14); `window.NostrTools` (`SimplePool`, `verifyEvent`).
- Produces:
  - `relays.js`: `export function currentRelays()` (parse `?relays=`, else defaults), `export function setRelays(list)` (updates URL via `history.replaceState`, re-runs discovery), `export function renderRelayEditor(container, onChange)`.
  - `render.js`: `export function operatorCard(entry, handlers)` returns an element; `export function badge(state)` for `"verified" | "mismatch" | "unreachable" | "checking"`.
  - `app.js`: `startDiscovery()` — connects the pool, streams events, verifies signatures with `NostrTools.verifyEvent(event)`, validates, upserts, re-renders `#operators` sorted by origin; kicks off `verifyDomain` per listed domain and updates badges in place; updates `#relay-status` chips (`connected` green / `error` red per relay). Card "Register" button calls `handlers.onRegister(entry, domain)`; "Register on operator's site" link when `registration-api-v1` missing.

- [ ] **Step 1: Implement `relays.js`:**

```js
import { DEFAULT_RELAYS } from "./config.js";

export function currentRelays() {
  const param = new URLSearchParams(location.search).get("relays");
  if (!param) return [...DEFAULT_RELAYS];
  const relays = param.split(",").map(r => r.trim()).filter(r => r.startsWith("wss://"));
  return relays.length ? relays : [...DEFAULT_RELAYS];
}

export function setRelays(relays) {
  const url = new URL(location.href);
  url.searchParams.set("relays", relays.join(","));
  history.replaceState(null, "", url);
}

export function renderRelayEditor(container, onChange) {
  container.replaceChildren();
  for (const relay of currentRelays()) {
    const chip = document.createElement("span");
    chip.className = "inline-flex items-center gap-1 rounded-full bg-gray-100 px-3 py-1 text-xs font-mono";
    chip.textContent = relay;
    const remove = document.createElement("button");
    remove.textContent = "×";
    remove.className = "text-gray-500 hover:text-red-600";
    remove.onclick = () => { setRelays(currentRelays().filter(r => r !== relay)); onChange(); };
    chip.append(remove);
    container.append(chip);
  }
  const input = document.createElement("input");
  input.placeholder = "wss://…";
  input.className = "rounded-lg border border-gray-300 bg-gray-50 p-1.5 text-xs font-mono";
  input.onkeydown = (e) => {
    if (e.key === "Enter" && input.value.startsWith("wss://")) {
      setRelays([...currentRelays(), input.value.trim()]);
      onChange();
    }
  };
  container.append(input);
}
```

- [ ] **Step 2: Implement `render.js`** — `badge(state)` returns a `<span>` with Flowbite badge classes (green `bg-green-100 text-green-800` for verified, red for mismatch, gray "?" for unreachable, blue "…" for checking). `operatorCard(entry, handlers)` builds a white card (`rounded-lg border border-gray-200 bg-white p-5 shadow-sm`) with: name (or origin) as `text-lg font-semibold`, `about` paragraph (`text-sm text-gray-500`, `textContent` — never innerHTML with remote data), a domain list where each row shows the domain, its badge (element id `badge-${pubkey}-${domain}` for in-place updates), the `priceSummary` string, and a Register button (`bg-blue-700 …` if `capabilities.includes("registration-api-v1")`, otherwise an `<a>` to `registration_url` with `target="_blank" rel="noopener"`), plus a footer line with contact (`<a href="nostr:npub…">` — convert hex pubkey via `NostrTools.nip19.npubEncode`), terms link, and `new Date(created_at*1000).toLocaleDateString()`.
- [ ] **Step 3: Implement discovery in `app.js`:**

```js
import { ANNOUNCEMENT_KIND, ANNOUNCEMENT_TAG } from "./config.js";
import { validateAnnouncement, upsertByCoordinate } from "./announcement.js";
import { currentRelays, renderRelayEditor } from "./relays.js";
import { operatorCard, badge } from "./render.js";

const operators = new Map();
let pool = null;

function startDiscovery() {
  if (pool) pool.close(currentRelays());
  operators.clear();
  pool = new NostrTools.SimplePool();
  const now = Math.floor(Date.now() / 1000);
  pool.subscribeMany(currentRelays(), [{ kinds: [ANNOUNCEMENT_KIND], "#t": [ANNOUNCEMENT_TAG] }], {
    onevent(event) {
      if (!NostrTools.verifyEvent(event)) return;
      const validated = validateAnnouncement(event, now);
      const before = operators.get(`${event.pubkey}:${validated.dtag ?? ""}`);
      upsertByCoordinate(operators, validated, event);
      renderOperators();
      if (validated.ok && !before) verifyDomains(validated, event);
    },
  });
  renderRelayStatus();
}

async function verifyDomains(validated, event) {
  for (const domain of validated.announcement.domains) {
    updateBadge(event.pubkey, domain, "checking");
    updateBadge(event.pubkey, domain, await verifyDomain(domain, event.pubkey, validated.dtag));
  }
}

async function verifyDomain(domain, pubkey, dtag) {
  try {
    const response = await fetch(`https://${domain}/.well-known/lnaddrd.json`, { signal: AbortSignal.timeout(5000) });
    if (!response.ok) return "unreachable";
    const doc = await response.json();
    return doc.schema === 1 && doc.service_pubkey === pubkey &&
      doc.announcement === `30078:${pubkey}:${dtag}` ? "verified" : "mismatch";
  } catch { return "unreachable"; }
}
```

`renderOperators()` sorts `[...operators.values()]` by `validated.origin` and rebuilds `#operators` with `operatorCard`. `renderRelayStatus()` renders one chip per relay; hook per-relay state via `pool.subscribeMany`'s `oneose`/`onclose` callbacks (mark a relay green on eose, red on close with reason). Wire `renderRelayEditor(document.getElementById("relay-editor"), startDiscovery)` and call `startDiscovery()` on load. Keep the tab-switching code from Task 13.
- [ ] **Step 4: Manual verification** — `just marketplace-serve`; run a local operator (`just run wss://<somerelay>` with `LNADDRD_PUBLIC_BASE_URL` unset it won't announce, so test against public relays or a local relay + published announcement). Minimum bar: page loads with no console errors, relay chips render, editor edits update `?relays=` in the URL. If a real announcement exists on the default relays, its card renders.
- [ ] **Step 5: Commit** — `feat(marketplace): relay discovery and operator cards`

---

### Task 16: Registration modal — quote, free and paid flows

**Files:**
- Create: `marketplace/js/api.js`, `marketplace/js/modal.js`
- Modify: `marketplace/js/app.js` (wire `onRegister`)

**Interfaces:**
- Consumes: API v1 endpoint shapes from Task 6 (exact field names), `qrcode` global.
- Produces `api.js` (all functions take `origin` and return parsed JSON or throw `Error` with the server's `error` code):

```js
export async function quote(origin, domain, username)              // GET  /api/v1/register/quote
export async function registerFree(origin, body)                   // POST /api/v1/register
export async function registerStart(origin, body)                  // POST /api/v1/register/start
export async function registerStatus(origin, id)                   // GET  /api/v1/register/{id}
export async function apiFetch(url, options)                       // shared: throws Error(json.error ?? statusText)
```

  and `modal.js`: `export function openRegisterModal({ origin, domain, ownerPubkey })` — full flow; `export function closeModal()`.

- [ ] **Step 1: Implement `api.js`:**

```js
export async function apiFetch(url, options = {}) {
  const response = await fetch(url, { ...options, headers: { "content-type": "application/json", ...(options.headers ?? {}) } });
  const body = await response.json().catch(() => ({}));
  if (!response.ok) throw new Error(body.error ?? `HTTP ${response.status}`);
  return body;
}
export const quote = (origin, domain, username) =>
  apiFetch(`${origin}/api/v1/register/quote?domain=${encodeURIComponent(domain)}&username=${encodeURIComponent(username)}`);
export const registerFree = (origin, body) =>
  apiFetch(`${origin}/api/v1/register`, { method: "POST", body: JSON.stringify(body) });
export const registerStart = (origin, body) =>
  apiFetch(`${origin}/api/v1/register/start`, { method: "POST", body: JSON.stringify(body) });
export const registerStatus = (origin, id) => apiFetch(`${origin}/api/v1/register/${id}`);
```

- [ ] **Step 2: Implement `modal.js`** — renders into `#modal-root` a fixed overlay (`fixed inset-0 bg-gray-900/50 flex items-center justify-center p-4`) containing a white card (`w-full max-w-lg rounded-lg bg-white p-6 shadow-lg space-y-4`). Contents:
  - Title `Register @${domain}`, close button.
  - Username input; on input, debounce 400 ms → `quote()`; render "This name is free." / `Price: N sats` / the error code as a red `role="alert"` line; disable submit while unresolved.
  - Destination textarea (LNURL or Lightning Address), same styling as `src/ui.rs` inputs.
  - Submit (`bg-blue-700 …`): if last quote was 0 → `registerFree(origin, {domain, username, destination, ...(ownerPubkey && {owner_pubkey: ownerPubkey})})`, then render success view: address, management token in a `<pre class="bg-gray-100 rounded p-3 text-sm break-all select-all">` with a Copy button (`navigator.clipboard.writeText`) and red "Store this token now — it is shown once." If priced → `registerStart(...)`, then render the invoice: amount in sats, QR (`qrcode(0,"L")`, `addData(bolt11.toUpperCase())`, `createSvgTag({cellSize:4,margin:4})`), `<pre>` with the bolt11 and a Copy button, countdown to `expires_at`; poll `registerStatus` every 3 s: `pending_payment` keeps waiting, `publishing` shows "Paid — waiting for relay acknowledgement…", `complete` swaps in the success view, `expired` shows an alert and stops.
  - All error paths render the message inline; never store anything.
- [ ] **Step 3: Wire it** — in `app.js`, pass `onRegister: (entry, domain) => openRegisterModal({ origin: entry.validated.origin, domain, ownerPubkey: connectedPubkey })` (`connectedPubkey` is `null` until Task 17).
- [ ] **Step 4: Manual e2e** — run a local operator with CORS build from Task 7 on `localhost:8080`; because announcements need HTTPS origins, test the modal directly from the browser console: `openRegisterModal({origin: "http://localhost:8080", domain: "localhost", ownerPubkey: null})` (temporarily export it on `window` for the test). Register a free name end-to-end; confirm token renders and Copy works.
- [ ] **Step 5: Commit** — `feat(marketplace): in-page registration flow`

---

### Task 17: Manage tab, NIP-07 connect, NIP-98 signing, README

**Files:**
- Create: `marketplace/js/nostr-auth.js`, `marketplace/js/manage.js`
- Modify: `marketplace/js/app.js`, `marketplace/js/api.js`, `README.md`

**Interfaces:**
- Consumes: `window.nostr` (NIP-07), `/api/v1/addresses` + management endpoints (Task 11 shapes).
- Produces:

```js
// nostr-auth.js
export async function connect()            // window.nostr.getPublicKey(); throws if no extension
export async function nip98Header(url, method, body /* string | undefined */)
// api.js additions
export const listAddresses = (origin, authHeader) => apiFetch(`${origin}/api/v1/addresses`, { headers: { authorization: authHeader } });
export function updateAddress(origin, body, authHeader?)   // PUT  /lnaddress/update
export function removeAddress(origin, body, authHeader?)   // DELETE /lnaddress/remove
```

- [ ] **Step 1: Implement `nostr-auth.js`:**

```js
export async function connect() {
  if (!window.nostr) throw new Error("No NIP-07 extension found");
  return await window.nostr.getPublicKey();
}

async function sha256Hex(text) {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text));
  return [...new Uint8Array(digest)].map(b => b.toString(16).padStart(2, "0")).join("");
}

export async function nip98Header(url, method, body) {
  const tags = [["u", url], ["method", method]];
  if (body !== undefined) tags.push(["payload", await sha256Hex(body)]);
  const event = await window.nostr.signEvent({
    kind: 27235, created_at: Math.floor(Date.now() / 1000), tags, content: "",
  });
  return `Nostr ${btoa(JSON.stringify(event))}`;
}
```

- [ ] **Step 2: Wire "Connect Nostr"** in `app.js`: click → `connect()` → store `connectedPubkey` in memory, button text becomes `npub…` short form (`NostrTools.nip19.npubEncode(pk).slice(0, 12) + "…"`); registrations now carry `owner_pubkey`.
- [ ] **Step 3: Implement `manage.js`** rendering into `#manage`:
  - Operator picker: dropdown of discovered operators (from the shared `operators` map).
  - If connected: "Load my addresses" → `nip98Header(origin + "/api/v1/addresses", "GET")` → `listAddresses` → table (address, destination, Update / Delete buttons). Update prompts for a new destination (inline input) and calls `updateAddress` with a NIP-98 header signed over the full URL and body; Delete asks `confirm()` first.
  - Token fallback (always visible): inputs for domain, username, management token, new destination; Update/Delete buttons calling the same endpoints with `{..., authentication_token}` bodies and no auth header.
  - `updateAddress`/`removeAddress` in `api.js`: `PUT ${origin}/lnaddress/update` / `DELETE ${origin}/lnaddress/remove`, JSON bodies `{domain, username, destination, authentication_token?}` / `{domain, username, authentication_token?}`, optional `authorization` header.
- [ ] **Step 4: Manual e2e** against the local operator: register with a NIP-07 extension connected, load addresses in Manage, update destination, delete. Also verify the token fallback path.
- [ ] **Step 5: README** — add a "Marketplace" section: what it is, `just marketplace-serve`, hosting (any static host), `?relays=` parameter, statelessness note, pointer to docs/protocol 02 + 03.
- [ ] **Step 6: Full check** — `cargo fmt --all && cargo clippy --all --all-targets -- -D warnings && cargo test --all && just marketplace-test`.
- [ ] **Step 7: Commit** — `feat(marketplace): manage tab with NIP-07/NIP-98 auth`
