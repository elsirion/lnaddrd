import test from "node:test";
import assert from "node:assert/strict";
import { priceForLength, formatSats, tierSummary } from "../js/pricing.js";

// -- priceForLength ---------------------------------------------------------

test("priceForLength: missing tiers array returns 0", () => {
  assert.equal(priceForLength(undefined, 5), 0);
  assert.equal(priceForLength(null, 5), 0);
});

test("priceForLength: empty tiers array returns 0", () => {
  assert.equal(priceForLength([], 5), 0);
});

test("priceForLength: picks first tier (ascending) with max_length >= length", () => {
  const tiers = [
    { max_length: 2, price: 1000000 },
    { max_length: 4, price: 100000 },
    { max_length: 64, price: 0 },
  ];
  assert.equal(priceForLength(tiers, 1), 1000000);
  assert.equal(priceForLength(tiers, 2), 1000000);
  assert.equal(priceForLength(tiers, 3), 100000);
  assert.equal(priceForLength(tiers, 4), 100000);
  assert.equal(priceForLength(tiers, 5), 0);
  assert.equal(priceForLength(tiers, 64), 0);
});

test("priceForLength: does not assume sorted input", () => {
  const tiers = [
    { max_length: 64, price: 0 },
    { max_length: 4, price: 100000 },
    { max_length: 2, price: 1000000 },
  ];
  assert.equal(priceForLength(tiers, 3), 100000);
  assert.equal(priceForLength(tiers, 1), 1000000);
});

test("priceForLength: length beyond every tier's max_length is 0 (no catch-all)", () => {
  const tiers = [{ max_length: 10, price: 5000 }];
  assert.equal(priceForLength(tiers, 10), 5000);
  assert.equal(priceForLength(tiers, 11), 0);
  assert.equal(priceForLength(tiers, 64), 0);
});

test("priceForLength: 64 catch-all tier matches every possible length", () => {
  const tiers = [
    { max_length: 4, price: 100000 },
    { max_length: 64, price: 1 },
  ];
  assert.equal(priceForLength(tiers, 64), 1);
  assert.equal(priceForLength(tiers, 5), 1);
});

// -- formatSats ---------------------------------------------------------

test("formatSats: 0 msat is free", () => {
  assert.equal(formatSats(0), "free");
});

test("formatSats: whole sats have no decimal point", () => {
  assert.equal(formatSats(1000000), "1000 sats");
  assert.equal(formatSats(1000), "1 sats");
});

test("formatSats: trims trailing decimal zeros", () => {
  assert.equal(formatSats(1500), "1.5 sats");
  assert.equal(formatSats(1050), "1.05 sats");
  assert.equal(formatSats(1), "0.001 sats");
});

test("formatSats: thousands are plain digits, no grouping separators", () => {
  assert.equal(formatSats(1234567000), "1234567 sats");
});

// -- tierSummary ---------------------------------------------------------

test("tierSummary: empty/absent tiers is free", () => {
  assert.equal(tierSummary(undefined), "free");
  assert.equal(tierSummary(null), "free");
  assert.equal(tierSummary([]), "free");
});

test("tierSummary: docs example — ranges plus open-ended 64 catch-all", () => {
  const tiers = [
    { max_length: 2, price: 1000000 },
    { max_length: 4, price: 100000 },
    { max_length: 64, price: 0 },
  ];
  assert.equal(tierSummary(tiers), "1–2: 1000 sats · 3–4: 100 sats · 5+: free");
});

test("tierSummary: unsorted input is sorted before summarizing", () => {
  const tiers = [
    { max_length: 64, price: 0 },
    { max_length: 2, price: 1000000 },
    { max_length: 4, price: 100000 },
  ];
  assert.equal(tierSummary(tiers), "1–2: 1000 sats · 3–4: 100 sats · 5+: free");
});

test("tierSummary: no-tier trailing range (tiers stop short of 64)", () => {
  const tiers = [{ max_length: 10, price: 5000 }];
  assert.equal(tierSummary(tiers), "1–10: 5 sats · 11+: free");
});

test("tierSummary: single 64 catch-all tier renders fully open-ended", () => {
  const tiers = [{ max_length: 64, price: 21000 }];
  assert.equal(tierSummary(tiers), "1+: 21 sats");
});
