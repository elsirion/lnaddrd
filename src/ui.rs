use crate::AppState;
use crate::api::RegisterRequest;
use crate::repository::DestinationPaymentAddress;
use axum::{
    Form,
    extract::Path,
    extract::State,
    response::{Html, IntoResponse, Redirect},
};
use maud::{DOCTYPE, Markup, html};
use qrcode::QrCode;
use qrcode::render::svg;
use serde::Deserialize;

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
            link rel="stylesheet" href="https://cdn.jsdelivr.net/npm/flowbite@1.7.0/dist/flowbite.min.css";
            script src="https://cdn.tailwindcss.com" {}
            script src="https://cdn.jsdelivr.net/npm/flowbite@1.7.0/dist/flowbite.min.js" {}
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
                    form id="register-form" method="post" action="/ui/register" class="space-y-6"  {
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
                            input name="username" id="username" required class="block w-full p-2.5 border border-gray-300 rounded-lg bg-gray-50 text-gray-900 focus:ring-blue-500 focus:border-blue-500" {}
                        }
                        div {
                            label for="lnurl" class="block mb-2 text-sm font-medium text-gray-900" { "LNURL or Lightning Address" }
                            textarea name="lnurl" id="lnurl" required rows="3" class="block w-full p-2.5 border border-gray-300 rounded-lg bg-gray-50 text-gray-900 focus:ring-blue-500 focus:border-blue-500 resize-y" style="word-break: break-all;" {}
                        }
                        button type="submit" class="w-full text-white bg-blue-700 hover:bg-blue-800 focus:ring-4 focus:outline-none focus:ring-blue-300 font-medium rounded-lg text-sm px-5 py-2.5 text-center" { "Register" }
                    }
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
        .register_lnaddr(&req.domain, &req.username, &req.lnurl)
        .await
    {
        Ok(_resp) => {
            // Redirect to the details page
            Redirect::to(&format!("/ui/lnaddress/{}/{}", req.domain, req.username)).into_response()
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
                    div class="text-center" {
                        a href="/" class="inline-block text-blue-600 hover:underline font-medium text-lg" { "Back to Register" }
                    }
                }
            }
        }
    };
    Ok(Html(markup.into_string()).into_response())
}
