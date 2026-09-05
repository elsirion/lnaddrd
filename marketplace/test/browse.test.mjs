import test from "node:test";
import assert from "node:assert/strict";
import { buildRows, filterRows, sortRows } from "../js/browse.js";

function operator(overrides = {}) {
  return {
    origin: "https://pay.example.com",
    name: "Example Operator",
    pubkey: "pubkey-example",
    capabilities: ["registration-api-v1", "nostr-auth"],
    verifiedDomains: ["pay.example.com"],
    pricing: [
      {
        domain: "pay.example.com",
        currency: "msat",
        tiers: [
          { max_length: 4, price: 100000 },
          { max_length: 64, price: 0 },
        ],
      },
    ],
    usersCount: 10,
    usersApprox: false,
    ...overrides,
  };
}

// -- buildRows ---------------------------------------------------------

test("buildRows: one row per verified domain, carrying operator-level fields", () => {
  const rows = buildRows([operator()]);
  assert.equal(rows.length, 1);
  assert.deepEqual(rows[0], {
    domain: "pay.example.com",
    origin: "https://pay.example.com",
    operatorName: "Example Operator",
    pubkey: "pubkey-example",
    canRegister: true,
    tiers: [
      { max_length: 4, price: 100000 },
      { max_length: 64, price: 0 },
    ],
    usersCount: 10,
    usersApprox: false,
    usersSource: "observed",
  });
});

test("buildRows: multiple verified domains produce multiple rows sharing operator fields", () => {
  const rows = buildRows([
    operator({
      verifiedDomains: ["a.example.com", "b.example.com"],
      pricing: [
        { domain: "a.example.com", currency: "msat", tiers: [{ max_length: 64, price: 500 }] },
      ],
    }),
  ]);
  assert.equal(rows.length, 2);
  assert.equal(rows[0].domain, "a.example.com");
  assert.deepEqual(rows[0].tiers, [{ max_length: 64, price: 500 }]);
  assert.equal(rows[1].domain, "b.example.com");
  assert.deepEqual(rows[1].tiers, []); // no pricing entry for this domain -> []
});

test("buildRows: canRegister requires both registration-api-v1 and nostr-auth", () => {
  assert.equal(buildRows([operator({ capabilities: ["registration-api-v1"] })])[0].canRegister, false);
  assert.equal(buildRows([operator({ capabilities: ["nostr-auth"] })])[0].canRegister, false);
  assert.equal(buildRows([operator({ capabilities: [] })])[0].canRegister, false);
  assert.equal(buildRows([operator({ capabilities: undefined })])[0].canRegister, false);
});

test("buildRows: missing/non-array pricing yields empty tiers", () => {
  const rows = buildRows([operator({ pricing: undefined })]);
  assert.deepEqual(rows[0].tiers, []);
});

test("buildRows: no verified domains yields no rows", () => {
  assert.deepEqual(buildRows([operator({ verifiedDomains: [] })]), []);
});

test("buildRows: flattens rows across multiple operators", () => {
  const rows = buildRows([
    operator({ pubkey: "op1", verifiedDomains: ["one.example.com"], pricing: [] }),
    operator({ pubkey: "op2", verifiedDomains: ["two.example.com"], pricing: [] }),
  ]);
  assert.deepEqual(rows.map(r => r.domain), ["one.example.com", "two.example.com"]);
});

// -- buildRows: announced vs. observed user counts ----------------------
//
// Per docs/superpowers/plans/2026-09-05-announced-user-counts.md's Global
// Constraints: an announced per-domain count wins over the operator-wide
// scan count. An operator that announced a usable count for at least one
// domain never gets scanned at all (app.js), so any of its rows lacking
// their own announced entry get no number ("unavailable"), not the
// operator-wide scan figure and not "loading".

test("buildRows: announced per-domain count takes precedence over the operator-wide scan count", () => {
  const rows = buildRows([
    operator({ usersCount: 999, usersApprox: true, userCounts: { "pay.example.com": 7 } }),
  ]);
  assert.equal(rows.length, 1);
  assert.equal(rows[0].usersCount, 7);
  assert.equal(rows[0].usersApprox, false);
  assert.equal(rows[0].usersSource, "announced");
});

test("buildRows: operator with no userCounts falls back to the operator-wide scan count", () => {
  const rows = buildRows([operator({ usersCount: 42, usersApprox: true, userCounts: {} })]);
  assert.equal(rows[0].usersCount, 42);
  assert.equal(rows[0].usersApprox, true);
  assert.equal(rows[0].usersSource, "observed");
});

test("buildRows: still-loading scan count (undefined) is reported as observed, not announced", () => {
  const rows = buildRows([operator({ usersCount: undefined, usersApprox: undefined, userCounts: undefined })]);
  assert.equal(rows[0].usersCount, undefined);
  assert.equal(rows[0].usersSource, "observed");
});

test("buildRows: mixed operator — one domain announced, the other not — the other gets 'unavailable', not the scan count", () => {
  const rows = buildRows([
    operator({
      verifiedDomains: ["a.example.com", "b.example.com"],
      pricing: [],
      usersCount: 500, // never actually populated for a skipped operator in
      usersApprox: true, // real app.js wiring, but set here to prove buildRows
      // ignores it once any domain has an announced count.
      userCounts: { "a.example.com": 3 },
    }),
  ]);
  const a = rows.find(r => r.domain === "a.example.com");
  const b = rows.find(r => r.domain === "b.example.com");
  assert.equal(a.usersCount, 3);
  assert.equal(a.usersApprox, false);
  assert.equal(a.usersSource, "announced");
  assert.equal(b.usersCount, undefined);
  assert.equal(b.usersSource, "unavailable");
});

test("buildRows: an announced zero count is kept (not treated as missing)", () => {
  const rows = buildRows([operator({ userCounts: { "pay.example.com": 0 } })]);
  assert.equal(rows[0].usersCount, 0);
  assert.equal(rows[0].usersSource, "announced");
});

// -- filterRows ---------------------------------------------------------

function row(overrides = {}) {
  return {
    domain: "pay.example.com",
    origin: "https://pay.example.com",
    operatorName: "Example Operator",
    pubkey: "pubkey-example",
    canRegister: true,
    tiers: [],
    usersCount: 10,
    usersApprox: false,
    ...overrides,
  };
}

test("filterRows: no filters returns all rows unchanged", () => {
  const rows = [row(), row({ domain: "other.example.com" })];
  assert.deepEqual(filterRows(rows, {}), rows);
});

test("filterRows: query matches domain case-insensitively", () => {
  const rows = [row({ domain: "Pay.Example.com" }), row({ domain: "other.example.com" })];
  const result = filterRows(rows, { query: "pay.example" });
  assert.deepEqual(result.map(r => r.domain), ["Pay.Example.com"]);
});

test("filterRows: query matches operatorName case-insensitively", () => {
  const rows = [row({ operatorName: "Acme Corp" }), row({ operatorName: "Other" })];
  const result = filterRows(rows, { query: "acme" });
  assert.deepEqual(result.map(r => r.operatorName), ["Acme Corp"]);
});

test("filterRows: name filter keeps only zero-price rows for that name's length", () => {
  const cheap = row({ domain: "cheap.example.com", tiers: [{ max_length: 64, price: 0 }] });
  const paid = row({ domain: "paid.example.com", tiers: [{ max_length: 64, price: 1000 }] });
  const result = filterRows([cheap, paid], { name: "alice" });
  assert.deepEqual(result.map(r => r.domain), ["cheap.example.com"]);
});

test("filterRows: name filter excludes unavailable (null-price) rows, not just non-zero ones", () => {
  const free = row({ domain: "free.example.com", tiers: [{ max_length: 64, price: 0 }] });
  // No tier covers a 5-char name here -> priceForLength returns null (unavailable).
  const unavailable = row({ domain: "unavailable.example.com", tiers: [{ max_length: 3, price: 0 }] });
  const result = filterRows([free, unavailable], { name: "alice" });
  assert.deepEqual(result.map(r => r.domain), ["free.example.com"]);
});

test("filterRows: name filter uses the name's own length against tier boundaries", () => {
  const rows = [
    row({ domain: "a.example.com", tiers: [{ max_length: 3, price: 0 }, { max_length: 64, price: 1000 }] }),
  ];
  // "bob" has length 3 -> first tier matches -> price 0 -> kept
  assert.deepEqual(filterRows(rows, { name: "bob" }).map(r => r.domain), ["a.example.com"]);
  // "alice" has length 5 -> falls to second tier -> price 1000 -> dropped
  assert.deepEqual(filterRows(rows, { name: "alice" }).map(r => r.domain), []);
});

test("filterRows: query and name combine with AND semantics", () => {
  const rows = [
    row({ domain: "free.example.com", operatorName: "Acme", tiers: [{ max_length: 64, price: 0 }] }),
    row({ domain: "paid.example.com", operatorName: "Acme", tiers: [{ max_length: 64, price: 1000 }] }),
    row({ domain: "free.other.com", operatorName: "Zeta", tiers: [{ max_length: 64, price: 0 }] }),
  ];
  const result = filterRows(rows, { query: "acme", name: "alice" });
  assert.deepEqual(result.map(r => r.domain), ["free.example.com"]);
});

test("filterRows: empty-string query/name are treated as no filter", () => {
  const rows = [row(), row({ domain: "other.example.com" })];
  assert.deepEqual(filterRows(rows, { query: "", name: "" }), rows);
});

// -- sortRows ---------------------------------------------------------

test("sortRows: alpha sorts by domain ascending", () => {
  const rows = [row({ domain: "b.example.com" }), row({ domain: "a.example.com" })];
  assert.deepEqual(sortRows(rows, { by: "alpha" }).map(r => r.domain), [
    "a.example.com",
    "b.example.com",
  ]);
});

test("sortRows: is pure and does not mutate the input array", () => {
  const rows = [row({ domain: "b.example.com" }), row({ domain: "a.example.com" })];
  const original = [...rows];
  sortRows(rows, { by: "alpha" });
  assert.deepEqual(rows, original);
});

test("sortRows: price sorts ascending by priceForLength at the given length", () => {
  const rows = [
    row({ domain: "expensive.example.com", tiers: [{ max_length: 64, price: 5000 }] }),
    row({ domain: "cheap.example.com", tiers: [{ max_length: 64, price: 100 }] }),
    row({ domain: "free.example.com", tiers: [{ max_length: 64, price: 0 }] }),
  ];
  const result = sortRows(rows, { by: "price", length: 5 });
  assert.deepEqual(result.map(r => r.domain), [
    "free.example.com",
    "cheap.example.com",
    "expensive.example.com",
  ]);
});

test("sortRows: price sort puts unavailable (null-price) rows last, ties still alphabetical", () => {
  const rows = [
    // No tier covers length 5 -> priceForLength(_, 5) is null (unavailable).
    row({ domain: "z-unavailable.example.com", tiers: [{ max_length: 3, price: 0 }] }),
    row({ domain: "cheap.example.com", tiers: [{ max_length: 64, price: 100 }] }),
    row({ domain: "a-unavailable.example.com", tiers: [{ max_length: 3, price: 0 }] }),
    row({ domain: "free.example.com", tiers: [{ max_length: 64, price: 0 }] }),
  ];
  const result = sortRows(rows, { by: "price", length: 5 });
  assert.deepEqual(result.map(r => r.domain), [
    "free.example.com",
    "cheap.example.com",
    "a-unavailable.example.com",
    "z-unavailable.example.com",
  ]);
});

test("sortRows: price tie-breaks by domain A-Z", () => {
  const rows = [
    row({ domain: "b.example.com", tiers: [{ max_length: 64, price: 100 }] }),
    row({ domain: "a.example.com", tiers: [{ max_length: 64, price: 100 }] }),
  ];
  const result = sortRows(rows, { by: "price", length: 5 });
  assert.deepEqual(result.map(r => r.domain), ["a.example.com", "b.example.com"]);
});

test("sortRows: users sorts descending, missing counts last, ties by domain A-Z", () => {
  const rows = [
    row({ domain: "few.example.com", usersCount: 5 }),
    row({ domain: "unknown-b.example.com", usersCount: undefined }),
    row({ domain: "many.example.com", usersCount: 100 }),
    row({ domain: "unknown-a.example.com", usersCount: undefined }),
    row({ domain: "tied.example.com", usersCount: 100 }),
  ];
  const result = sortRows(rows, { by: "users" }).map(r => r.domain);
  assert.deepEqual(result, [
    "many.example.com",
    "tied.example.com",
    "few.example.com",
    "unknown-a.example.com",
    "unknown-b.example.com",
  ]);
});

test("sortRows: alpha tie-breaks equal domains stably (no crash, deterministic)", () => {
  const rows = [
    row({ domain: "same.example.com", pubkey: "op2" }),
    row({ domain: "same.example.com", pubkey: "op1" }),
  ];
  const result = sortRows(rows, { by: "alpha" });
  assert.equal(result.length, 2);
  assert.ok(result.every(r => r.domain === "same.example.com"));
});
