use anyhow::Result;
use api::{
    get_lnaddr_handler, get_lnaddr_manifest_handler, list_domains_handler, register_lnaddr_handler,
    remove_lnaddr_handler,
};
use axum::{
    Router,
    response::IntoResponse,
    routing::{get, post},
};
use config::Config;
use repository::pg::PgPaymentAddressRepository;
use service::LnaddrService;
use service::direct::DirectLnaddrService;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::{debug, info};
use ui::{lnaddress_details, register_form, register_form_submit};

pub mod api;
pub mod config;
pub mod repository;
pub mod service;
pub mod ui;

#[derive(Clone)]
pub struct AppState {
    pub service: LnaddrService,
    pub config: Arc<Config>,
}

pub async fn serve(config: &Config) -> Result<()> {
    debug!(db=%config.database, "Opening database connection");
    let lnaddr_repo = PgPaymentAddressRepository::new(&config.database)?.into_dyn();

    debug!(domains=?config.domains, "Starting LN address service");
    let lnaddr_service = DirectLnaddrService::new(lnaddr_repo, config.domains.clone()).into_dyn();

    let app_state = AppState {
        service: lnaddr_service.clone(),
        config: Arc::new(config.clone()),
    };

    let app = Router::new()
        .route("/domains", get(list_domains_handler))
        .route("/lnaddress/:domain/:username", get(get_lnaddr_handler))
        .route("/lnaddress/register", post(register_lnaddr_handler))
        .route("/lnaddress/remove", post(remove_lnaddr_handler))
        .route(
            "/.well-known/lnurlp/:username",
            get(get_lnaddr_manifest_handler),
        )
        .route("/", get(register_form))
        .route("/ui/register", post(register_form_submit))
        .route("/ui/lnaddress/:domain/:username", get(lnaddress_details))
        .with_state(app_state)
        .fallback(|_req: axum::http::Request<axum::body::Body>| async move {
            axum::http::StatusCode::NOT_FOUND
        })
        .layer(axum::middleware::map_response(
            |res: axum::response::Response| async {
                if res.status().is_client_error() || res.status().is_server_error() {
                    let status = res.status();
                    let body = format!(
                        r#"<!DOCTYPE html>
                    <html>
                        <head><title>{} {}</title></head>
                        <body>
                            <h1>{} {}</h1>
                        </body>
                    </html>"#,
                        status.as_u16(),
                        status.canonical_reason().unwrap_or("Unknown"),
                        status.as_u16(),
                        status.canonical_reason().unwrap_or("Unknown")
                    );
                    return (
                        status,
                        [(axum::http::header::CONTENT_TYPE, "text/html")],
                        body,
                    )
                        .into_response();
                }
                res
            },
        ));

    info!(bind=%config.bind, "Starting HTTP server");
    let listener = TcpListener::bind(&config.bind).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
