// Thin client for the operator's registration API (docs/protocol/03-registration-api.md).
// Every function takes the operator's origin first and either returns the
// parsed JSON body or throws an Error whose message is the server's `error`
// code (or an HTTP status string if the body wasn't JSON / had no `error`).

export async function apiFetch(url, options = {}) {
  const response = await fetch(url, {
    ...options,
    headers: { "content-type": "application/json", ...(options.headers ?? {}) },
  });
  const body = await response.json().catch(() => ({}));
  if (!response.ok) throw new Error(body.error ?? `HTTP ${response.status}`);
  return body;
}

export const quote = (origin, domain, username) =>
  apiFetch(`${origin}/api/v1/register/quote?domain=${encodeURIComponent(domain)}&username=${encodeURIComponent(username)}`);

// Exported so callers that must sign the exact request URL (NIP-98's `u`
// tag) build it identically to what this module fetches — the two can never
// drift apart because there's only one place the path is spelled out.
export const registerFreeUrl = (origin) => `${origin}/api/v1/register`;
export const registerStartUrl = (origin) => `${origin}/api/v1/register/start`;

// `bodyString` must be the exact, pre-serialized JSON string the caller
// signed as the NIP-98 payload — this module sends it verbatim rather than
// re-serializing an object, since re-serializing could produce a different
// string (key order, whitespace) than the one that was hashed.
export const registerFree = (origin, bodyString, authHeader) =>
  apiFetch(registerFreeUrl(origin), { method: "POST", body: bodyString, headers: { authorization: authHeader } });

export const registerStart = (origin, bodyString, authHeader) =>
  apiFetch(registerStartUrl(origin), { method: "POST", body: bodyString, headers: { authorization: authHeader } });

export const registerStatus = (origin, id) => apiFetch(`${origin}/api/v1/register/${id}`);

// GET /api/v1/addresses requires a NIP-98 header; there is no fallback (see
// docs/protocol/03-registration-api.md).
export const listAddresses = (origin, authHeader) =>
  apiFetch(`${origin}/api/v1/addresses`, { headers: { authorization: authHeader } });

// The two endpoints below are the legacy `/lnaddress` surface: unlike
// `/api/v1`, their error responses are bare HTTP status codes with no JSON
// body at all (not even `{"error": "..."}`), so apiFetch's generic
// `HTTP <status>` fallback message is what callers see on failure — manage.js
// maps those statuses to endpoint-specific human text. Both accept either a
// NIP-98 `authHeader` (proof of ownership) or a body `authentication_token`;
// callers pass exactly one.

export const updateAddress = (origin, body, authHeader) =>
  apiFetch(`${origin}/lnaddress/update`, {
    method: "PUT",
    body: JSON.stringify(body),
    headers: authHeader ? { authorization: authHeader } : {},
  });

export const removeAddress = (origin, body, authHeader) =>
  apiFetch(`${origin}/lnaddress/remove`, {
    method: "DELETE",
    body: JSON.stringify(body),
    headers: authHeader ? { authorization: authHeader } : {},
  });
