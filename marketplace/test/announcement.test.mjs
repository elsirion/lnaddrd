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
