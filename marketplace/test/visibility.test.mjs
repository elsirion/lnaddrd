import test from "node:test";
import assert from "node:assert/strict";
import { classifyOperator, reconcileDomainStatuses } from "../js/visibility.js";

function statusMap(map) {
  return domain => map[domain] ?? "checking";
}

// Unlike statusMap(), returns undefined (not "checking") for domains with
// no prior entry — matching how app.js looks up a *previous* per-pubkey
// status before reconciling, where "no entry" genuinely means "never seen"
// rather than "in flight".
function previousStatus(map) {
  return domain => map[domain];
}

test("visible when at least one domain verified", () => {
  const result = classifyOperator(
    ["a.example.com", "b.example.com"],
    statusMap({ "a.example.com": "mismatch", "b.example.com": "verified" })
  );
  assert.equal(result.category, "visible");
  assert.deepEqual(result.verified, ["b.example.com"]);
});

test("verified list keeps original domain order and excludes unverified ones", () => {
  const result = classifyOperator(
    ["c.example.com", "a.example.com", "b.example.com"],
    statusMap({ "c.example.com": "verified", "a.example.com": "verified", "b.example.com": "unreachable" })
  );
  assert.equal(result.category, "visible");
  assert.deepEqual(result.verified, ["c.example.com", "a.example.com"]);
});

test("hidden when all domains settled with zero verified", () => {
  const result = classifyOperator(
    ["a.example.com", "b.example.com"],
    statusMap({ "a.example.com": "mismatch", "b.example.com": "unreachable" })
  );
  assert.equal(result.category, "hidden");
  assert.deepEqual(result.verified, []);
});

test("pending while any domain check is still in flight and none verified yet", () => {
  const result = classifyOperator(
    ["a.example.com", "b.example.com"],
    statusMap({ "a.example.com": "mismatch" }) // b.example.com defaults to "checking"
  );
  assert.equal(result.category, "pending");
  assert.deepEqual(result.verified, []);
});

test("pending (not hidden) for a domain with no status recorded yet at all", () => {
  const result = classifyOperator(["a.example.com"], statusMap({}));
  assert.equal(result.category, "pending");
});

test("visible takes priority even if other domains are still checking", () => {
  const result = classifyOperator(
    ["a.example.com", "b.example.com"],
    statusMap({ "a.example.com": "verified" }) // b.example.com still "checking"
  );
  assert.equal(result.category, "visible");
  assert.deepEqual(result.verified, ["a.example.com"]);
});

// --- reconcileDomainStatuses: stale-but-correct carry-over on republish ---

test("reconcile keeps a domain's last status untouched (stale-but-correct)", () => {
  const next = reconcileDomainStatuses(
    ["a.example.com"],
    previousStatus({ "a.example.com": "verified" })
  );
  assert.deepEqual([...next], [["a.example.com", "verified"]]);
});

test("reconcile carries over a failed status too, not just verified", () => {
  const next = reconcileDomainStatuses(
    ["a.example.com"],
    previousStatus({ "a.example.com": "mismatch" })
  );
  assert.equal(next.get("a.example.com"), "mismatch");
});

test("reconcile seeds a genuinely new domain at checking", () => {
  const next = reconcileDomainStatuses(
    ["a.example.com", "new.example.com"],
    previousStatus({ "a.example.com": "verified" })
  );
  assert.equal(next.get("a.example.com"), "verified");
  assert.equal(next.get("new.example.com"), "checking");
});

test("reconcile drops a domain no longer in the new announcement", () => {
  // Caller passes only the *new* domain list, so a domain removed from the
  // announcement is simply absent from the result even if it had a status.
  const next = reconcileDomainStatuses(
    ["a.example.com"],
    previousStatus({ "a.example.com": "verified", "removed.example.com": "verified" })
  );
  assert.deepEqual([...next.keys()], ["a.example.com"]);
});

test("reconcile on an entirely unseen coordinate starts every domain at checking", () => {
  const next = reconcileDomainStatuses(["a.example.com", "b.example.com"], previousStatus({}));
  assert.deepEqual([...next.values()], ["checking", "checking"]);
});
