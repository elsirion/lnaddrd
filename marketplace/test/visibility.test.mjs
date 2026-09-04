import test from "node:test";
import assert from "node:assert/strict";
import { classifyOperator } from "../js/visibility.js";

function statusMap(map) {
  return domain => map[domain] ?? "checking";
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
