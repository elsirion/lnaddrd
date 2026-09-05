// Fetches per-operator registered-user counts from public backup records.
//
// Backup records (docs/protocol/01-private-backup-records.md) are addressable
// `kind:30078` events, one per registered address, authored by the
// operator's service pubkey with `d` = BACKUP_D_PREFIX + <opaque hash>. The
// same kind is reused by that operator's service announcement (`d` prefix
// "lnaddrd:service:v1:") and its private config record (`d` exactly
// "lnaddrd:config:v1") — NIP-01 filters can't match a `d` prefix
// server-side, so a query for `{kinds:[30078], authors:[...]}` returns all
// three kinds of event for these authors, and the backup ones are picked out
// client-side here.
//
// Because a relay's `limit` caps the *raw* per-filter result (announcements
// + config + backups together), a relay returning exactly COUNT_QUERY_LIMIT
// events means it may be holding back more of ANY of those events, for ANY
// author in the query — there's no way to tell, from a capped response
// alone, whose events got cut. So hitting the limit marks every pubkey in
// that query's batch as approximate, not just the ones the returned events
// happened to mention.

import { ANNOUNCEMENT_KIND, BACKUP_D_PREFIX, COUNT_QUERY_LIMIT } from "./config.js";

function dTag(event) {
  const tag = Array.isArray(event?.tags) ? event.tags.find(t => Array.isArray(t) && t[0] === "d") : null;
  return typeof tag?.[1] === "string" ? tag[1] : null;
}

/**
 * Pure, dependency-free accumulator for backup-record counts — no DOM, no
 * pool, no relay I/O — so it can be unit tested directly.
 *
 * Feed it raw `kind:30078` events (any author, any `d`) via `addEvent`; it
 * keeps only events whose `d` starts with BACKUP_D_PREFIX and dedupes by
 * `pubkey + d` so the same backup record delivered by multiple relays (or
 * redelivered by the same relay) is only counted once. `markApprox` records
 * that some relay's query hit COUNT_QUERY_LIMIT while a pubkey was part of
 * the queried batch. `snapshot(pubkey)` returns that pubkey's current
 * `{count, approx}` (count 0, approx false if never seen).
 */
export function createCountAggregator() {
  const seenKeys = new Set();
  const counts = new Map();
  const approxPubkeys = new Set();

  return {
    addEvent(event) {
      if (!event || event.kind !== ANNOUNCEMENT_KIND) return;
      const d = dTag(event);
      if (!d || !d.startsWith(BACKUP_D_PREFIX)) return;
      const key = `${event.pubkey}:${d}`;
      if (seenKeys.has(key)) return;
      seenKeys.add(key);
      counts.set(event.pubkey, (counts.get(event.pubkey) ?? 0) + 1);
    },
    markApprox(pubkey) {
      approxPubkeys.add(pubkey);
    },
    snapshot(pubkey) {
      return { count: counts.get(pubkey) ?? 0, approx: approxPubkeys.has(pubkey) };
    },
  };
}

/**
 * Queries every relay independently (one `subscribeMany([relay], ...)` call
 * per relay, mirroring app.js's per-relay discovery subscriptions — so one
 * slow or dead relay can't hold back another's results) for `kind:30078`
 * events authored by `pubkeys`, aggregates them via createCountAggregator,
 * and calls `onUpdate(pubkey, count, approx)` for every pubkey in the batch
 * each time a relay's query settles (eose or close). Counts and the approx
 * flag only grow more complete/more pessimistic over the life of the call —
 * `onUpdate` may fire once per relay per pubkey as results come in.
 *
 * This function fetches exactly the batch it's given, once; it does not
 * decide *which* pubkeys are worth (re-)fetching or debounce repeated calls
 * — that policy (fetch once per newly discovered operator pubkey, not per
 * keystroke) lives in app.js, which owns the set of already-counted
 * pubkeys.
 *
 * Returns `{ close() }` to cancel the underlying per-relay subscriptions
 * (e.g. when discovery restarts against a different relay set).
 */
export function fetchBackupCounts(pool, relays, pubkeys, onUpdate) {
  const batch = [...new Set(pubkeys)].filter(Boolean);
  if (batch.length === 0 || !Array.isArray(relays) || relays.length === 0) {
    return { close() {} };
  }

  const aggregator = createCountAggregator();
  const filter = { kinds: [ANNOUNCEMENT_KIND], authors: batch, limit: COUNT_QUERY_LIMIT };
  const subs = [];

  for (const relay of relays) {
    let rawCount = 0;
    let settled = false;
    // Declared before `sub` is assigned since `finish` (passed into
    // subscribeMany below) can fire synchronously from within that same
    // call; `sub` is only read once finish actually runs, by which point the
    // assignment below has completed.
    let sub;
    const finish = () => {
      if (settled) return;
      settled = true;
      // >= rather than === defends against a relay that ignores `limit` and
      // returns more events than requested; both cases mean the response
      // can't be trusted as exhaustive.
      if (rawCount >= COUNT_QUERY_LIMIT) {
        for (const pubkey of batch) aggregator.markApprox(pubkey);
      }
      for (const pubkey of batch) {
        const { count, approx } = aggregator.snapshot(pubkey);
        onUpdate(pubkey, count, approx);
      }
      // This relay's query is done (eose or close) and has delivered its
      // final onUpdate for this batch — close its REQ now rather than
      // leaving it open for the rest of the session. Without this, every
      // newly discovered operator batch would leak one open subscription
      // per relay, eventually hitting relays' per-connection subscription
      // caps. (Bulk close() below still covers cancelling a batch mid-flight,
      // e.g. when discovery restarts against a different relay set.)
      sub?.close();
    };
    sub = pool.subscribeMany([relay], [filter], {
      onevent(event) {
        rawCount++;
        if (NostrTools.verifyEvent(event)) aggregator.addEvent(event);
      },
      oneose: finish,
      onclose: finish,
    });
    subs.push(sub);
  }

  return {
    close() {
      subs.forEach(sub => sub.close());
    },
  };
}
