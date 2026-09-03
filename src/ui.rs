use crate::AppState;
use crate::api::RegisterRequest;
use crate::registration::{Quote, RegistrationStatus};
use crate::repository::DestinationPaymentAddress;
use axum::{
    Form,
    extract::{ConnectInfo, Path, State},
    response::{Html, IntoResponse},
};
use maud::{DOCTYPE, Markup, html};
use qrcode::QrCode;
use qrcode::render::svg;
use serde::Deserialize;
use std::net::SocketAddr;

#[derive(Debug, Deserialize)]
pub struct QuoteForm {
    domain: String,
    username: String,
}

#[derive(Debug, Deserialize)]
pub struct StartForm {
    domain: String,
    username: String,
    destination: String,
}

pub async fn registration_quote(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Form(form): Form<QuoteForm>,
) -> impl IntoResponse {
    if !state
        .registration_manager
        .allow_request(peer.ip(), "quote", 30)
        .await
    {
        return (
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            Html("Rate limit exceeded".to_owned()),
        )
            .into_response();
    }
    match state
        .registration_manager
        .quote(&form.domain, &form.username)
        .await
    {
        Ok(Quote::Free) => {
            Html(html! { p role="status" { "This name is free." } }.into_string()).into_response()
        }
        Ok(Quote::Paid(amount)) => {
            Html(html! { p role="status" { "Price: " (amount) " msat." } }.into_string())
                .into_response()
        }
        Err(error) => (
            axum::http::StatusCode::BAD_REQUEST,
            Html(html! { p role="alert" { (error) } }.into_string()),
        )
            .into_response(),
    }
}

pub async fn registration_start(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Form(form): Form<StartForm>,
) -> impl IntoResponse {
    if !state
        .registration_manager
        .allow_request(peer.ip(), "start", 10)
        .await
    {
        return (
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            Html("Rate limit exceeded".to_owned()),
        )
            .into_response();
    }
    match state
        .registration_manager
        .quote(&form.domain, &form.username)
        .await
    {
        Ok(Quote::Free) => match state
            .service
            .register_lnaddr(&form.domain, &form.username, &form.destination, None)
            .await
        {
            Ok(response) => Html(
                html! {
                    p { "Address: " (response.lnaddr) }
                    p { "Management token (shown once): " code { (response.authentication_token) } }
                    @if !response.active { p { "Waiting for a Nostr relay acknowledgement." } }
                }
                .into_string(),
            )
            .into_response(),
            Err(error) => registration_fragment_error(error),
        },
        Ok(Quote::Paid(_)) => match state
            .registration_manager
            .start(&form.domain, &form.username, &form.destination, None)
            .await
        {
            Ok(started) => {
                let invoice_qr = QrCode::new(started.invoice.as_bytes())
                    .ok()
                    .map(|code| code.render::<svg::Color>().min_dimensions(256, 256).build());
                Html(html! {
                    div hx-get=(format!("/register/{}/status", started.id)) hx-trigger="every 3s" hx-swap="outerHTML" {
                        p { "Pay " (started.amount_msat) " msat before " (started.expires_at) "." }
                        @if let Some(invoice_qr) = invoice_qr { (maud::PreEscaped(invoice_qr)) }
                        pre style="overflow-wrap:anywhere" { (started.invoice) }
                        p { "Payment is checked by the server using LUD-21." }
                    }
                }.into_string()).into_response()
            }
            Err(error) => registration_fragment_error(error),
        },
        Err(error) => registration_fragment_error(error),
    }
}

pub async fn registration_status(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.registration_manager.status(&id).await {
        Ok(RegistrationStatus::Pending) => Html(html! {
            div hx-get=(format!("/register/{id}/status")) hx-trigger="every 3s" hx-swap="outerHTML" { p { "Waiting for payment…" } }
        }.into_string()).into_response(),
        Ok(RegistrationStatus::Publishing) => Html(html! {
            div hx-get=(format!("/register/{id}/status")) hx-trigger="every 5s" hx-swap="outerHTML" { p { "Paid. Waiting for a Nostr relay acknowledgement…" } }
        }.into_string()).into_response(),
        Ok(RegistrationStatus::Complete { address, management_token }) => Html(html! {
            div {
                p { "Address: " (address) }
                @if let Some(management_token) = management_token {
                    p { "Management token (shown once): " code { (management_token) } }
                } @else {
                    p { "The management token was already displayed." }
                }
            }
        }.into_string()).into_response(),
        Ok(RegistrationStatus::Expired) => Html(html! { p role="alert" { "This payment attempt expired." } }.into_string()).into_response(),
        Err(error) => Html(html! {
            div hx-get=(format!("/register/{id}/status")) hx-trigger="every 5s" hx-swap="outerHTML" {
                p role="status" { "Payment verification is temporarily unavailable: " (error) }
            }
        }.into_string()).into_response(),
    }
}

fn registration_fragment_error(error: anyhow::Error) -> axum::response::Response {
    (
        axum::http::StatusCode::BAD_REQUEST,
        Html(html! { p role="alert" { (error) } }.into_string()),
    )
        .into_response()
}

const LNURL_GENERATOR_JS: &str = r#"
(function() {
    var C = window.LNURL_CONFIG;
    var minSats = Math.ceil(C.minMsat / 1000);
    var maxSats = Math.floor(C.maxMsat / 1000);

    document.getElementById('range-display').textContent = minSats + ' \u2013 ' + maxSats + ' sats';

    var input = document.getElementById('amount-sats');
    input.min = minSats;
    input.max = maxSats;

    document.getElementById('generate-btn').addEventListener('click', async function() {
        var errorEl = document.getElementById('amount-error');
        var resultEl = document.getElementById('lnurl-result');
        var sats = parseInt(input.value);

        if (isNaN(sats) || input.value === '') {
            errorEl.textContent = 'Please enter a valid amount';
            errorEl.classList.remove('hidden');
            resultEl.classList.add('hidden');
            return;
        }

        var msat = sats * 1000;
        if (msat < C.minMsat || msat > C.maxMsat) {
            errorEl.textContent = 'Amount must be between ' + minSats + ' and ' + maxSats + ' sats';
            errorEl.classList.remove('hidden');
            resultEl.classList.add('hidden');
            return;
        }

        errorEl.classList.add('hidden');

        try {
            var resp = await fetch('/lnurl/' + C.username + '?min_sendable=' + msat + '&max_sendable=' + msat);
            if (!resp.ok) throw new Error('Server returned ' + resp.status);
            var data = await resp.json();
            var lnurl = data.lnurl;

            document.getElementById('lnurl-text').textContent = lnurl;

            var qrEl = document.getElementById('lnurl-qr');
            if (typeof qrcode === 'function') {
                var qr = qrcode(0, 'L');
                qr.addData(lnurl);
                qr.make();
                qrEl.innerHTML = qr.createSvgTag({cellSize: 4, margin: 4});
            }

            resultEl.classList.remove('hidden');
        } catch(e) {
            errorEl.textContent = 'Failed to generate LNURL: ' + e.message;
            errorEl.classList.remove('hidden');
            resultEl.classList.add('hidden');
        }
    });
})();
"#;

#[derive(Deserialize)]
pub struct RegisterForm {
    domain: String,
    username: String,
    lnurl: String,
}

// Add a helper function for the common <head> markup
fn common_head(title: &str) -> Markup {
    html! {
        head {
            meta charset="UTF-8";
            meta name="viewport" content="width=device-width, initial-scale=1.0";
            title { (title) }
            link rel="stylesheet" href="/assets/flowbite-1.7.0.min.css";
            script src="/assets/tailwindcss-3.4.17.js" {}
            script src="/assets/flowbite-1.7.0.min.js" {}
            script src="/assets/htmx-4.0.0.min.js" {}
        }
    }
}

pub async fn register_form(State(state): State<AppState>) -> impl IntoResponse {
    let domains = state.service.list_domains().await.unwrap_or_default();
    let warning = state.config.warning.clone();
    let markup = html! {
        (DOCTYPE)
        html lang="en" {
            (common_head("Register LN Address"))
            body class="bg-gray-50 min-h-screen flex items-center justify-center" {
                div class="w-full max-w-lg mx-auto p-6 bg-white rounded-lg shadow-lg" {
                    h1 class="text-3xl font-bold mb-6 text-center text-gray-900" { "Register LN Address" }
                    @if let Some(warning) = warning {
                        div class="p-4 mb-4 text-sm text-yellow-800 rounded-lg bg-yellow-50 dark:bg-gray-800 dark:text-yellow-300" role="alert" {
                            span class="font-bold" { "Warning:" }
                            " " (maud::PreEscaped(warning))
                        }
                    }
                    form id="register-form" method="post" action="/register/start" class="space-y-6"
                        hx-post="/register/start" hx-target="#registration-result" hx-swap="innerHTML" {
                        div {
                            label for="domain" class="block mb-2 text-sm font-medium text-gray-900" { "Domain" }
                            select name="domain" id="domain" required class="block w-full p-2.5 border border-gray-300 rounded-lg bg-gray-50 text-gray-900 focus:ring-blue-500 focus:border-blue-500" {
                                @for domain in &domains {
                                    option value=(domain) { (domain) }
                                }
                            }
                        }
                        div {
                            label for="username" class="block mb-2 text-sm font-medium text-gray-900" { "Username" }
                            input name="username" id="username" required class="block w-full p-2.5 border border-gray-300 rounded-lg bg-gray-50 text-gray-900 focus:ring-blue-500 focus:border-blue-500"
                                hx-post="/register/quote" hx-trigger="keyup changed delay:400ms" hx-target="#registration-quote" hx-include="#domain" {}
                            div id="registration-quote" {}
                        }
                        div {
                            label for="lnurl" class="block mb-2 text-sm font-medium text-gray-900" { "LNURL or Lightning Address" }
                            textarea name="destination" id="lnurl" required rows="3" class="block w-full p-2.5 border border-gray-300 rounded-lg bg-gray-50 text-gray-900 focus:ring-blue-500 focus:border-blue-500 resize-y" style="word-break: break-all;" {}
                        }
                        p { "Paid registrations buy this exact claim attempt. If backup publication is delayed, it will retry without another payment. Refunds are not provided." }
                        button type="submit" class="w-full text-white bg-blue-700 hover:bg-blue-800 focus:ring-4 focus:outline-none focus:ring-blue-300 font-medium rounded-lg text-sm px-5 py-2.5 text-center" { "Register" }
                    }
                    div id="registration-result" {}
                    div class="flex justify-center mt-10" {
                        a href="https://github.com/elsirion/lnaddrd" target="_blank" rel="noopener noreferrer" class="flex items-center space-x-2 text-gray-600 hover:text-black transition-colors" {
                            (maud::PreEscaped(r#"<svg xmlns='http://www.w3.org/2000/svg' fill='currentColor' viewBox='0 0 24 24' class='w-6 h-6'><path d='M12 0C5.37 0 0 5.373 0 12c0 5.303 3.438 9.8 8.205 11.387.6.113.82-.258.82-.577 0-.285-.01-1.04-.015-2.04-3.338.726-4.042-1.61-4.042-1.61-.546-1.387-1.333-1.756-1.333-1.756-1.09-.745.083-.729.083-.729 1.205.085 1.84 1.237 1.84 1.237 1.07 1.834 2.807 1.304 3.492.997.108-.775.418-1.305.762-1.606-2.665-.304-5.466-1.334-5.466-5.931 0-1.31.468-2.381 1.236-3.221-.124-.303-.535-1.523.117-3.176 0 0 1.008-.322 3.3 1.23a11.52 11.52 0 0 1 3.003-.404c1.02.005 2.047.138 3.003.404 2.291-1.553 3.297-1.23 3.297-1.23.653 1.653.242 2.873.118 3.176.77.84 1.235 1.911 1.235 3.221 0 4.609-2.804 5.625-5.475 5.921.43.372.823 1.102.823 2.222 0 1.606-.014 2.898-.014 3.293 0 .322.218.694.825.576C20.565 21.796 24 17.299 24 12c0-6.627-5.373-12-12-12z'/></svg>"#))
                            span { "View on GitHub" }
                        }
                    }
                }
            }
        }
    };
    Html(markup.into_string())
}

pub async fn register_form_submit(
    State(state): State<AppState>,
    Form(form): Form<RegisterForm>,
) -> impl IntoResponse {
    let req = RegisterRequest {
        domain: form.domain.clone(),
        username: form.username.clone(),
        lnurl: form.lnurl,
    };
    match state
        .service
        .register_lnaddr(&req.domain, &req.username, &req.lnurl, None)
        .await
    {
        Ok(resp) => {
            let status = if resp.active {
                "Your Lightning Address is active."
            } else {
                "Your address is reserved and will become active after a Nostr relay acknowledges its encrypted backup."
            };
            let markup = html! {
                (DOCTYPE)
                html lang="en" {
                    (common_head("Registration Result"))
                    body class="bg-gray-50 min-h-screen flex items-center justify-center" {
                        div class="w-full max-w-lg mx-auto p-6 bg-white rounded-lg shadow-lg" {
                            h1 class="text-2xl font-bold mb-4 text-center text-gray-900" { "Registration Result" }
                            p class="mb-4" { (status) }
                            p class="mb-2" { b { "Lightning Address:" } " " (resp.lnaddr) }
                            p class="mb-2" { b { "Management token (shown once):" } }
                            pre class="bg-gray-100 rounded p-3 text-sm break-all select-all mb-4" { (resp.authentication_token) }
                            p class="text-sm text-red-700 mb-4" { "Store this token now. It is required to update or delete the address." }
                            @if resp.active {
                                a href=(format!("/ui/lnaddress/{}/{}", req.domain, req.username)) class="inline-block text-blue-600 hover:underline" { "View address" }
                            } @else {
                                a href="/" class="inline-block text-blue-600 hover:underline" { "Back" }
                            }
                        }
                    }
                }
            };
            Html(markup.into_string()).into_response()
        }
        Err(e) => {
            let markup: Markup = html! {
                (DOCTYPE)
                html lang="en" {
                    (common_head("Error"))
                    body class="bg-gray-50 min-h-screen flex items-center justify-center" {
                        div class="w-full max-w-lg mx-auto p-6 bg-white rounded-lg shadow-lg" {
                            h1 class="text-2xl font-bold mb-4 text-center text-red-700" { "Error" }
                            div class="mb-6 text-center text-red-600 font-mono break-all" { (e.to_string()) }
                            div class="text-center" {
                                a href="/" class="inline-block text-white bg-blue-700 hover:bg-blue-800 focus:ring-4 focus:outline-none focus:ring-blue-300 font-medium rounded-lg text-sm px-5 py-2.5 text-center" { "Back to Register" }
                            }
                        }
                    }
                }
            };
            Html(markup.into_string()).into_response()
        }
    }
}

pub async fn lnaddress_details(
    State(state): State<AppState>,
    Path((domain, username)): Path<(String, String)>,
) -> Result<impl IntoResponse, axum::http::StatusCode> {
    let lnaddr = format!("{username}@{domain}");
    let destination_addr = state
        .service
        .get_destination(&domain, &username)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(axum::http::StatusCode::NOT_FOUND)?;
    let manifest = state
        .service
        .get_lnaddr_manifest(&domain, &username)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?
        .expect("If LNURL is registered, manifest should be present");
    let manifest_str = serde_json::to_string_pretty(&manifest).unwrap();

    let lnaddr_svg = {
        let lnaddr_code = QrCode::new(&lnaddr).unwrap();
        lnaddr_code
            .render::<svg::Color>()
            .min_dimensions(256, 256)
            .build()
    };

    let config_js = format!(
        "window.LNURL_CONFIG = {{ domain: {}, username: {}, minMsat: {}, maxMsat: {} }};",
        serde_json::to_string(&domain).unwrap(),
        serde_json::to_string(&username).unwrap(),
        manifest.min_sendable,
        manifest.max_sendable,
    );

    let markup = html! {
        (DOCTYPE)
        html lang="en" {
            (common_head("LN Address Details"))
            body class="bg-gray-50 min-h-screen flex items-center justify-center" {
                div class="w-full max-w-lg mx-auto p-6 bg-white rounded-lg shadow-lg" {
                    h1 class="text-3xl font-bold mb-6 text-center text-gray-900" { "LN Address Details" }
                    div class="mb-4" {
                        p class="mb-2" { b { "Lightning Address:" } " " (lnaddr) }
                        div class="flex justify-center mb-2" { (maud::PreEscaped(lnaddr_svg)) }
                        p class="mb-2" {
                            b {
                                (match &destination_addr {
                                    DestinationPaymentAddress::Lnurl(_) => "LNURL:",
                                    DestinationPaymentAddress::LnAddress { .. } => "LN Address:",
                                })
                            }
                            " " span class="break-all font-mono" { (destination_addr) }
                        }
                        p class="mb-2" { b { "Decoded:" } " " span class="break-all font-mono" { (destination_addr.url()) } }
                        p class="mb-2" { b { "Manifest:" } }
                        pre class="bg-gray-100 rounded p-2 text-xs overflow-x-auto" { (manifest_str) }
                    }
                    hr class="my-6 border-gray-300" {}
                    h2 class="text-2xl font-bold mb-4 text-gray-900" { "Generate Fixed-Amount LNURL" }
                    div class="mb-4" {
                        label for="amount-sats" class="block mb-2 text-sm font-medium text-gray-900" {
                            "Amount (sats) \u{2014} range: "
                            span id="range-display" {}
                        }
                        div class="flex space-x-2" {
                            input type="number" id="amount-sats"
                                class="block w-full p-2.5 border border-gray-300 rounded-lg bg-gray-50 text-gray-900 focus:ring-blue-500 focus:border-blue-500"
                                placeholder="Amount in sats" {}
                            button type="button" id="generate-btn"
                                class="text-white bg-blue-700 hover:bg-blue-800 focus:ring-4 focus:outline-none focus:ring-blue-300 font-medium rounded-lg text-sm px-5 py-2.5 whitespace-nowrap"
                                { "Generate" }
                        }
                        p id="amount-error" class="mt-1 text-sm text-red-600 hidden" {}
                    }
                    div id="lnurl-result" class="hidden mb-4" {
                        p class="mb-2" { b { "LNURL:" } }
                        div class="bg-gray-100 rounded p-2 mb-2" {
                            p id="lnurl-text" class="break-all font-mono text-xs select-all" {}
                        }
                        div id="lnurl-qr" class="flex justify-center mb-2" {}
                    }
                    div class="text-center" {
                        a href="/" class="inline-block text-blue-600 hover:underline font-medium text-lg" { "Back to Register" }
                    }
                }
                script src="https://cdn.jsdelivr.net/npm/qrcode-generator@1.4.4/qrcode.min.js" {}
                script { (maud::PreEscaped(&config_js)) }
                script { (maud::PreEscaped(LNURL_GENERATOR_JS)) }
            }
        }
    };
    Ok(Html(markup.into_string()).into_response())
}
