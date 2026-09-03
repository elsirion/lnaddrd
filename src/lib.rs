use admin::{
    AdminAuth, address_delete_submit, address_retry_submit, dashboard, domain_addresses,
    domain_settings, login_page, login_submit, logout_submit, payment_policy_submit,
    payment_policy_validate, profile_submit, reserved_name_submit, restore_dry_run_submit,
    seed_export_submit,
};
use anyhow::Result;
use api::{
    flowbite_css_asset_handler, flowbite_js_asset_handler, generate_lnurl_handler,
    get_lnaddr_handler, get_lnaddr_manifest_handler, htmx_asset_handler, list_domains_handler,
    liveness_handler, readiness_handler, register_lnaddr_handler, remove_lnaddr_handler,
    tailwind_asset_handler, update_lnaddr_handler, well_known_announcement_handler,
};
use api_v1::{addresses_v1, quote_v1, register_start_v1, register_status_v1, register_v1};
use axum::{
    Router,
    extract::DefaultBodyLimit,
    http::Method,
    routing::{delete, get, post, put},
};
use config::Config;
use configuration::ConfigurationManager;
use crypto::{RootSecret, ServiceKeys};
use nostr::announcement::AnnouncementWorker;
use nostr::http_auth::Nip98ReplayGuard;
use nostr::publisher::{NostrPublisher, OutboxWorker, Publisher};
use nostr::restore;
use registration::RegistrationManager;
use repository::sqlite::SqlitePaymentAddressRepository;
use service::LnaddrService;
use service::direct::DirectLnaddrService;
use std::sync::Arc;
use tokio::net::TcpListener;
use tower_http::cors::{Any, CorsLayer};
use tracing::{debug, info};
use ui::{
    lnaddress_details, register_form, register_form_submit, registration_quote, registration_start,
    registration_status,
};

pub mod admin;
pub mod api;
pub mod api_v1;
pub mod bootstrap;
pub mod config;
pub mod configuration;
pub mod crypto;
pub mod domain;
#[cfg(feature = "postgres-import")]
pub mod import_postgres;
pub mod nostr;
pub mod outbound;
pub mod payment;
pub mod registration;
pub mod repository;
pub mod service;
pub mod ui;

#[derive(Clone)]
pub struct AppState {
    pub service: LnaddrService,
    pub config: Arc<Config>,
    pub keys: Arc<ServiceKeys>,
    pub repository: SqlitePaymentAddressRepository,
    pub admin_auth: Arc<AdminAuth>,
    pub configuration_manager: Arc<ConfigurationManager>,
    pub registration_manager: Arc<RegistrationManager>,
    pub publisher: Publisher,
    pub nostr: Arc<NostrPublisher>,
    pub nip98_guard: Arc<Nip98ReplayGuard>,
}

pub async fn serve(config: &Config) -> Result<()> {
    let restart = Arc::new(tokio::sync::Notify::new());
    loop {
        debug!(path=%config.database_path, "Opening SQLite database");
        let repository = SqlitePaymentAddressRepository::new(&config.database_path)?;
        let admin_auth = Arc::new(AdminAuth::load_or_create(
            &config.admin_password_file,
            repository.clone(),
        )?);
        let initialized = repository.metadata("initialized").await?.as_deref() == Some("true");
        let app = if initialized {
            normal_router(config, repository, admin_auth).await?
        } else {
            info!("Database is uninitialized; exposing authenticated setup UI only");
            bootstrap::router(bootstrap::BootstrapState {
                config: Arc::new(config.clone()),
                admin_auth,
                restart: restart.clone(),
            })
            .layer(DefaultBodyLimit::max(16 * 1024))
        };

        info!(bind=%config.bind, initialized, "Starting HTTP server");
        let listener = TcpListener::bind(&config.bind).await?;
        let shutdown = restart.clone();
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .with_graceful_shutdown(async move { shutdown.notified().await })
        .await?;
        if initialized {
            break;
        }
        info!("Setup complete; switching to normal service mode");
    }
    Ok(())
}

async fn normal_router(
    config: &Config,
    lnaddr_repo: SqlitePaymentAddressRepository,
    admin_auth: Arc<AdminAuth>,
) -> Result<Router> {
    let keys = Arc::new(RootSecret::load(&config.root_secret_file)?.derive()?);
    info!(service_pubkey=%keys.service_public_key(), "Loaded service identity");
    let nostr = Arc::new(NostrPublisher::connect(&config.nostr_relays).await?);
    let publisher: Publisher = nostr.clone();
    let configuration_manager = Arc::new(ConfigurationManager::new(
        lnaddr_repo.clone(),
        keys.clone(),
        publisher.clone(),
        &config.domains,
    )?);
    let registration_manager = Arc::new(RegistrationManager::new(
        lnaddr_repo.clone(),
        &config.domains,
        keys.clone(),
        publisher.clone(),
    )?);

    debug!(domains=?config.domains, "Starting LN address service");
    let lnaddr_service = DirectLnaddrService::new(
        lnaddr_repo.clone(),
        config.domains.clone(),
        keys.clone(),
        publisher.clone(),
    )?
    .into_dyn();
    tokio::spawn(OutboxWorker::new(lnaddr_repo.clone(), publisher.clone()).run());
    tokio::spawn(
        AnnouncementWorker::new(
            config.clone(),
            lnaddr_repo.clone(),
            keys.clone(),
            publisher.clone(),
        )
        .run(),
    );

    let app_state = AppState {
        service: lnaddr_service.clone(),
        config: Arc::new(config.clone()),
        keys,
        repository: lnaddr_repo,
        admin_auth,
        configuration_manager,
        registration_manager,
        publisher,
        nostr,
        nip98_guard: Arc::new(Nip98ReplayGuard::new()),
    };

    Ok(build_router(app_state))
}

/// CORS policy applied to the public API and public LN address/LNURL endpoints:
/// any origin may call them, including via a preflight `OPTIONS` request.
fn cors_layer() -> CorsLayer {
    CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([
            axum::http::header::CONTENT_TYPE,
            axum::http::header::AUTHORIZATION,
        ])
}

/// Builds the full application `Router` from an already-assembled `AppState`.
///
/// Routes are split into two sub-routers so CORS can be scoped precisely:
/// `public` (the JSON API under `/api/v1`, the LN address/LNURL endpoints, the
/// domain list, and the announcement document) gets [`cors_layer`] and answers
/// preflight requests from any origin; `private` (the admin panel, the HTMX/UI
/// pages, static assets, and health checks) does not. Shared by both the real
/// server (via `normal_router`) and the test harness (`api_v1::tests::test_router`)
/// so the CORS wiring under test is the same code path production runs.
pub(crate) fn build_router(app_state: AppState) -> Router {
    let public = Router::new()
        .route("/domains", get(list_domains_handler))
        .route(
            "/.well-known/lnaddrd.json",
            get(well_known_announcement_handler),
        )
        .route("/lnaddress/:domain/:username", get(get_lnaddr_handler))
        .route("/lnaddress/register", post(register_lnaddr_handler))
        .route("/lnaddress/remove", delete(remove_lnaddr_handler))
        .route("/lnaddress/update", put(update_lnaddr_handler))
        .route(
            "/.well-known/lnurlp/:username",
            get(get_lnaddr_manifest_handler),
        )
        .route("/lnurl/:username", get(generate_lnurl_handler))
        .route("/api/v1/register/quote", get(quote_v1))
        .route("/api/v1/register", post(register_v1))
        .route("/api/v1/register/start", post(register_start_v1))
        .route("/api/v1/register/:id", get(register_status_v1))
        .route("/api/v1/addresses", get(addresses_v1))
        .layer(cors_layer());

    let private = Router::new()
        .route("/health/live", get(liveness_handler))
        .route("/health/ready", get(readiness_handler))
        .route("/assets/htmx-4.0.0.min.js", get(htmx_asset_handler))
        .route("/assets/tailwindcss-3.4.17.js", get(tailwind_asset_handler))
        .route(
            "/assets/flowbite-1.7.0.min.css",
            get(flowbite_css_asset_handler),
        )
        .route(
            "/assets/flowbite-1.7.0.min.js",
            get(flowbite_js_asset_handler),
        )
        .route("/admin", get(dashboard))
        .route("/admin/domains/:domain/addresses", get(domain_addresses))
        .route("/admin/domains/:domain/settings", get(domain_settings))
        .route("/admin/login", get(login_page).post(login_submit))
        .route("/admin/logout", post(logout_submit))
        .route("/admin/reserved", post(reserved_name_submit))
        .route("/admin/payment-policy", post(payment_policy_submit))
        .route("/admin/profile", post(profile_submit))
        .route(
            "/admin/payment-policy/validate",
            post(payment_policy_validate),
        )
        .route("/admin/address/retry", post(address_retry_submit))
        .route("/admin/address/delete", post(address_delete_submit))
        .route("/admin/restore-dry-run", post(restore_dry_run_submit))
        .route("/admin/seed/export", post(seed_export_submit))
        .route("/", get(register_form))
        .route("/ui/register", post(register_form_submit))
        .route("/register/quote", post(registration_quote))
        .route("/register/start", post(registration_start))
        .route("/register/status/:id", get(registration_status))
        .route("/register/:id/status", get(registration_status))
        .route("/ui/lnaddress/:domain/:username", get(lnaddress_details));

    public
        .merge(private)
        .layer(DefaultBodyLimit::max(16 * 1024))
        .with_state(app_state)
        .fallback(|_req: axum::http::Request<axum::body::Body>| async move {
            axum::http::StatusCode::NOT_FOUND
        })
}

/// Test-only `AppState` builder for handler-level tests (see `src/api_v1.rs`). The
/// caller supplies the repository, domains, publisher and service so it can inject
/// fixtures (e.g. a no-op `DestinationValidator`); everything else that a real
/// `AppState` needs but that unit tests never exercise (admin auth, Nostr relay
/// client, CLI `Config`) is stubbed out here.
#[cfg(test)]
pub async fn test_app_state(
    repository: SqlitePaymentAddressRepository,
    domains: &[String],
    publisher: Publisher,
    service: LnaddrService,
) -> Result<AppState> {
    let keys = Arc::new(RootSecret::from_bytes([0x42; 32]).derive()?);
    let configuration_manager = Arc::new(ConfigurationManager::new(
        repository.clone(),
        keys.clone(),
        publisher.clone(),
        domains,
    )?);
    let registration_manager = Arc::new(RegistrationManager::new(
        repository.clone(),
        domains,
        keys.clone(),
        publisher.clone(),
    )?);
    let scratch = tempfile::tempdir()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(scratch.path(), std::fs::Permissions::from_mode(0o700))?;
    }
    let admin_auth = Arc::new(AdminAuth::load_or_create(
        &scratch.path().join("password"),
        repository.clone(),
    )?);
    // The admin password file only needs to exist while `load_or_create` reads it;
    // leak the directory so later test assertions never race its cleanup.
    std::mem::forget(scratch);
    let config = Arc::new(Config {
        operation: None,
        domains: domains.to_vec(),
        bind: "127.0.0.1:0".parse()?,
        database_path: String::new(),
        root_secret_file: std::path::PathBuf::new(),
        admin_password_file: std::path::PathBuf::new(),
        nostr_relays: Vec::new(),
        public_base_url: None,
        service_name: "test".to_owned(),
        warning: None,
    });
    let nostr = Arc::new(NostrPublisher::offline());

    Ok(AppState {
        service,
        config,
        keys,
        repository,
        admin_auth,
        configuration_manager,
        registration_manager,
        publisher,
        nostr,
        nip98_guard: Arc::new(Nip98ReplayGuard::new()),
    })
}

pub async fn initialize_empty(config: &Config) -> Result<()> {
    anyhow::ensure!(
        !config.domains.is_empty(),
        "At least one domain is required"
    );
    let keys = RootSecret::load_or_create(&config.root_secret_file)?.derive()?;
    let repository = SqlitePaymentAddressRepository::new(&config.database_path)?;
    let network = NostrPublisher::connect(&config.nostr_relays).await?;
    restore::initialize_empty(&repository, &network, &keys, &config.domains).await
}

pub async fn restore_database(config: &Config, dry_run: bool) -> Result<()> {
    anyhow::ensure!(
        !config.domains.is_empty(),
        "At least one domain is required"
    );
    let keys = Arc::new(RootSecret::load_or_create(&config.root_secret_file)?.derive()?);
    let repository = SqlitePaymentAddressRepository::new(&config.database_path)?;
    let network = NostrPublisher::connect(&config.nostr_relays).await?;
    let summary = restore::restore(&repository, &network, keys, &config.domains, dry_run).await?;
    info!(
        active_addresses = summary.active_addresses,
        tombstones = summary.tombstones,
        configuration_revision = summary.configuration_revision,
        dry_run,
        "Nostr restore complete"
    );
    Ok(())
}
