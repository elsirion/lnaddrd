export const DEFAULT_RELAYS = [
  "wss://relay.damus.io",
  "wss://nos.lol",
  "wss://relay.nostr.band",
];
export const ANNOUNCEMENT_KIND = 30078;
export const ANNOUNCEMENT_TAG = "lightning-address-service";
export const ANNOUNCEMENT_PREFIX = "lnaddrd:service:v1:";
// Private backup records (docs/protocol/01-private-backup-records.md) share
// ANNOUNCEMENT_KIND with announcements and the operator's config record; only
// the `d` tag prefix tells them apart. Used by counts.js to count an
// operator's registered addresses without decrypting anything.
export const BACKUP_D_PREFIX = "lnaddrd:backup:v1:";
// Per-relay query cap for counts.js's backup-record count query. A relay
// returning exactly this many events means its true count may be higher —
// see counts.js for how that turns into the "N+" approximate badge.
export const COUNT_QUERY_LIMIT = 1000;
