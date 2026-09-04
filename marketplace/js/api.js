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

export const registerFree = (origin, body) =>
  apiFetch(`${origin}/api/v1/register`, { method: "POST", body: JSON.stringify(body) });

export const registerStart = (origin, body) =>
  apiFetch(`${origin}/api/v1/register/start`, { method: "POST", body: JSON.stringify(body) });

export const registerStatus = (origin, id) => apiFetch(`${origin}/api/v1/register/${id}`);
