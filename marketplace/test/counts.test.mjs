import test from "node:test";
import assert from "node:assert/strict";
import { createCountAggregator } from "../js/counts.js";
import { ANNOUNCEMENT_KIND, BACKUP_D_PREFIX } from "../js/config.js";

function event(overrides = {}) {
  return {
    kind: ANNOUNCEMENT_KIND,
    pubkey: "pubkey-a",
    tags: [["d", `${BACKUP_D_PREFIX}${"11".repeat(32)}`]],
    ...overrides,
  };
}

test("createCountAggregator: counts a backup event for its pubkey", () => {
  const agg = createCountAggregator();
  agg.addEvent(event());
  assert.deepEqual(agg.snapshot("pubkey-a"), { count: 1, approx: false });
});

test("createCountAggregator: dedupes by pubkey + d across repeated/multi-relay deliveries", () => {
  const agg = createCountAggregator();
  const e = event();
  agg.addEvent(e);
  agg.addEvent(e); // same event redelivered by another relay
  agg.addEvent({ ...e, tags: [...e.tags] }); // structurally identical, different object
  assert.deepEqual(agg.snapshot("pubkey-a"), { count: 1, approx: false });
});

test("createCountAggregator: distinct d tags for the same pubkey each count", () => {
  const agg = createCountAggregator();
  agg.addEvent(event({ tags: [["d", `${BACKUP_D_PREFIX}${"11".repeat(32)}`]] }));
  agg.addEvent(event({ tags: [["d", `${BACKUP_D_PREFIX}${"22".repeat(32)}`]] }));
  assert.deepEqual(agg.snapshot("pubkey-a"), { count: 2, approx: false });
});

test("createCountAggregator: separates counts per pubkey", () => {
  const agg = createCountAggregator();
  agg.addEvent(event({ pubkey: "pubkey-a" }));
  agg.addEvent(event({ pubkey: "pubkey-b", tags: [["d", `${BACKUP_D_PREFIX}${"33".repeat(32)}`]] }));
  agg.addEvent(event({ pubkey: "pubkey-b", tags: [["d", `${BACKUP_D_PREFIX}${"44".repeat(32)}`]] }));
  assert.equal(agg.snapshot("pubkey-a").count, 1);
  assert.equal(agg.snapshot("pubkey-b").count, 2);
});

test("createCountAggregator: ignores the service announcement d prefix", () => {
  const agg = createCountAggregator();
  agg.addEvent(event({ tags: [["d", "lnaddrd:service:v1:somehash"]] }));
  assert.deepEqual(agg.snapshot("pubkey-a"), { count: 0, approx: false });
});

test("createCountAggregator: ignores the operator config record", () => {
  const agg = createCountAggregator();
  agg.addEvent(event({ tags: [["d", "lnaddrd:config:v1"]] }));
  assert.deepEqual(agg.snapshot("pubkey-a"), { count: 0, approx: false });
});

test("createCountAggregator: ignores events of a different kind", () => {
  const agg = createCountAggregator();
  agg.addEvent(event({ kind: 1 }));
  assert.deepEqual(agg.snapshot("pubkey-a"), { count: 0, approx: false });
});

test("createCountAggregator: ignores events with no d tag", () => {
  const agg = createCountAggregator();
  agg.addEvent(event({ tags: [] }));
  assert.deepEqual(agg.snapshot("pubkey-a"), { count: 0, approx: false });
});

test("createCountAggregator: tolerates null/undefined input", () => {
  const agg = createCountAggregator();
  assert.doesNotThrow(() => agg.addEvent(null));
  assert.doesNotThrow(() => agg.addEvent(undefined));
});

test("createCountAggregator: snapshot of an unseen pubkey is {count: 0, approx: false}", () => {
  const agg = createCountAggregator();
  assert.deepEqual(agg.snapshot("never-seen"), { count: 0, approx: false });
});

test("createCountAggregator: markApprox flags only the given pubkey", () => {
  const agg = createCountAggregator();
  agg.addEvent(event({ pubkey: "pubkey-a" }));
  agg.markApprox("pubkey-a");
  assert.equal(agg.snapshot("pubkey-a").approx, true);
  assert.equal(agg.snapshot("pubkey-b").approx, false);
});

test("createCountAggregator: markApprox is independent of and persists across further addEvent calls", () => {
  const agg = createCountAggregator();
  agg.markApprox("pubkey-a");
  agg.addEvent(event({ pubkey: "pubkey-a" }));
  assert.deepEqual(agg.snapshot("pubkey-a"), { count: 1, approx: true });
});
