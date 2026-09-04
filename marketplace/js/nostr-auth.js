// NIP-07 connect + NIP-98 HTTP auth header signing.
// Keys never leave the browser extension: this module only ever calls
// window.nostr, never touches private key material itself, and stores
// nothing (no localStorage/sessionStorage/cookies).

export async function connect() {
  if (!window.nostr) throw new Error("No NIP-07 extension found");
  return await window.nostr.getPublicKey();
}

async function sha256Hex(text) {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text));
  return [...new Uint8Array(digest)].map(b => b.toString(16).padStart(2, "0")).join("");
}

/**
 * Builds a NIP-98 `Authorization: Nostr <base64>` header value.
 * `url` must be the exact request URL (operator origin + path + query);
 * `body`, if given, must be the identical string passed as the fetch body —
 * the payload tag is a hash of that exact string.
 */
export async function nip98Header(url, method, body) {
  const tags = [["u", url], ["method", method]];
  if (body !== undefined) tags.push(["payload", await sha256Hex(body)]);
  const event = await window.nostr.signEvent({
    kind: 27235, created_at: Math.floor(Date.now() / 1000), tags, content: "",
  });
  return `Nostr ${btoa(JSON.stringify(event))}`;
}
