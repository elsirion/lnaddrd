import test from "node:test";
import assert from "node:assert/strict";
import { validateAnnouncement, upsertByCoordinate, isPublicHost } from "../js/announcement.js";

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
test("terms_url must use HTTPS", () => {
  // HTTP terms_url fails
  assert.equal(validateAnnouncement(makeEvent({}, { terms_url: "http://insecure.example/terms" }), 1500).ok, false);
  // HTTPS terms_url passes
  const result = validateAnnouncement(makeEvent({}, { terms_url: "https://ok.example/terms" }), 1500);
  assert.equal(result.ok, true);
});
test("expiration tag must be pure decimal integer", () => {
  // Malformed expiration tag with non-numeric suffix fails
  assert.equal(validateAnnouncement(makeEvent({ tags: [["d", `lnaddrd:service:v1:${ORIGIN}`], ["t", "lightning-address-service"], ["expiration", "2000xyz"]] }), 1500).ok, false);
});

// Shared vectors (verbatim from docs/superpowers/plans/2026-09-04-marketplace-hardening.md).
const PUBLIC_HOST_ACCEPT = ["lnaddr.org", "pay.lnaddr.org", "foo-bar.io", "a.b.co", "xn--ls8h.net", "svc.example2.com"];
const PUBLIC_HOST_REJECT = [
  "localhost", "foo.localhost", "mybox.local", "svc.internal", "demo.test", "x.invalid",
  "site.example", "1.2.3.4", "192.168.0.10", "[::1]", "::1", "lnaddr.org.", ".lnaddr.org",
  "foo..bar", "UPPER.org", "single", "-bad.org", "bad-.org", "foo.123",
];
test("isPublicHost matches shared test vectors", () => {
  for (const host of PUBLIC_HOST_ACCEPT) {
    assert.equal(isPublicHost(host), true, `expected ${host} to be accepted`);
  }
  for (const host of PUBLIC_HOST_REJECT) {
    assert.equal(isPublicHost(host), false, `expected ${host} to be rejected`);
  }
});
test("non-public origin host fails validation", () => {
  const origin = "https://localhost";
  const event = makeEvent(
    { tags: [["d", `lnaddrd:service:v1:${origin}`], ["t", "lightning-address-service"]] },
    { origin, registration_url: `${origin}/` }
  );
  const result = validateAnnouncement(event, 1500);
  assert.equal(result.ok, false);
  assert.equal(result.error, "Host is not public");
});
test("non-public domain entry fails validation", () => {
  const event = makeEvent({}, {
    domains: ["mybox.local"],
    pricing: [{ domain: "mybox.local", currency: "msat", tiers: [{ max_length: 64, price: 0 }] }],
  });
  const result = validateAnnouncement(event, 1500);
  assert.equal(result.ok, false);
  assert.equal(result.error, "Host is not public");
});

// -- users field sanitization (docs/protocol/02's "users" section) --------

test("absent users field yields an empty userCounts map, still valid", () => {
  const result = validateAnnouncement(makeEvent(), 1500);
  assert.equal(result.ok, true);
  assert.deepEqual(result.userCounts, {});
});

test("valid users entries are kept, including a zero count", () => {
  const event = makeEvent({}, {
    domains: ["pay.example.com", "tips.example.org"].sort(),
    users: [
      { domain: "pay.example.com", count: 42 },
      { domain: "tips.example.org", count: 0 },
    ],
  });
  const result = validateAnnouncement(event, 1500);
  assert.equal(result.ok, true);
  assert.deepEqual(result.userCounts, { "pay.example.com": 42, "tips.example.org": 0 });
});

test("users entry for a domain not in this announcement's domains is dropped", () => {
  const event = makeEvent({}, { users: [{ domain: "unannounced.example.com", count: 5 }] });
  const result = validateAnnouncement(event, 1500);
  assert.equal(result.ok, true);
  assert.deepEqual(result.userCounts, {});
});

test("users entry with a negative count is dropped", () => {
  const event = makeEvent({}, { users: [{ domain: "pay.example.com", count: -1 }] });
  const result = validateAnnouncement(event, 1500);
  assert.equal(result.ok, true);
  assert.deepEqual(result.userCounts, {});
});

test("users entry with a fractional count is dropped", () => {
  const event = makeEvent({}, { users: [{ domain: "pay.example.com", count: 1.5 }] });
  const result = validateAnnouncement(event, 1500);
  assert.equal(result.ok, true);
  assert.deepEqual(result.userCounts, {});
});

test("users entry with a count beyond MAX_SAFE_INTEGER is dropped", () => {
  const event = makeEvent({}, { users: [{ domain: "pay.example.com", count: 1e308 }] });
  const result = validateAnnouncement(event, 1500);
  assert.equal(result.ok, true);
  assert.deepEqual(result.userCounts, {});
});

test("users entry with a string count is dropped", () => {
  const event = makeEvent({}, { users: [{ domain: "pay.example.com", count: "42" }] });
  const result = validateAnnouncement(event, 1500);
  assert.equal(result.ok, true);
  assert.deepEqual(result.userCounts, {});
});

test("a malformed users field never invalidates the announcement, and yields an empty map", () => {
  for (const badUsers of ["not-an-array", 42, null, {}, [null], [{ domain: "pay.example.com" }], [{ count: 5 }]]) {
    const result = validateAnnouncement(makeEvent({}, { users: badUsers }), 1500);
    assert.equal(result.ok, true, `expected users=${JSON.stringify(badUsers)} to still validate`);
    assert.deepEqual(result.userCounts, {});
  }
});

test("one invalid users entry is dropped without discarding sibling valid entries", () => {
  const event = makeEvent({}, {
    domains: ["pay.example.com", "tips.example.org"].sort(),
    users: [
      { domain: "pay.example.com", count: 3 },
      { domain: "tips.example.org", count: -1 },
    ],
  });
  const result = validateAnnouncement(event, 1500);
  assert.equal(result.ok, true);
  assert.deepEqual(result.userCounts, { "pay.example.com": 3 });
});
