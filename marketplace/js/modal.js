import {
  quote as apiQuote,
  registerFree as apiRegisterFree,
  registerStart as apiRegisterStart,
  registerStatus as apiRegisterStatus,
} from "./api.js";

const INPUT_CLASS =
  "block w-full p-2.5 border border-gray-300 rounded-lg bg-gray-50 text-gray-900 focus:ring-blue-500 focus:border-blue-500";
const LABEL_CLASS = "block mb-2 text-sm font-medium text-gray-900";
const PRIMARY_BUTTON_CLASS =
  "w-full text-white bg-blue-700 hover:bg-blue-800 focus:ring-4 focus:ring-blue-300 font-medium rounded-lg text-sm px-5 py-2.5 disabled:opacity-50 disabled:cursor-not-allowed";
const SECONDARY_BUTTON_CLASS = "text-white bg-blue-700 hover:bg-blue-800 font-medium rounded-lg text-xs px-3 py-1.5";

// Maps the API's `{"error": "<code>"}` codes (docs/protocol/03-registration-api.md)
// to a human-readable sentence. The raw code is always appended in parens so
// the message stays diagnosable even when our copy doesn't fit the situation.
const ERROR_MESSAGES = {
  invalid_input: "That name or destination isn't valid.",
  unsupported_domain: "This domain isn't supported by the operator.",
  taken: "This name is already taken.",
  reserved: "This name is reserved.",
  length_disabled: "Registrations of this length aren't available.",
  payment_required: "This name now requires payment; refresh and try again.",
  free_registration: "This name is now free; refresh and try again.",
  owner_mismatch: "This address is already claimed by a different key.",
  unauthorized: "Not authorized.",
  rate_limited: "Too many requests — please wait a moment and try again.",
  not_found: "That registration attempt could not be found.",
  internal: "The operator hit an internal error.",
};

function describeError(message) {
  const known = ERROR_MESSAGES[message];
  return known ? `${known} (${message})` : `Something went wrong: ${message}`;
}

/** A red, role="alert" paragraph — the only way errors are ever surfaced. */
function alertParagraph(message) {
  const p = document.createElement("p");
  p.className = "text-sm text-red-700";
  p.setAttribute("role", "alert");
  p.textContent = message;
  return p;
}

function copyRow(text) {
  const row = document.createElement("div");
  row.className = "flex items-center gap-2";
  const copyBtn = document.createElement("button");
  copyBtn.type = "button";
  copyBtn.className = SECONDARY_BUTTON_CLASS;
  copyBtn.textContent = "Copy";
  const flash = document.createElement("span");
  flash.className = "text-xs text-green-700";
  copyBtn.addEventListener("click", async () => {
    try {
      await navigator.clipboard.writeText(text);
      flash.textContent = "Copied";
    } catch {
      flash.textContent = "Copy failed";
    }
    setTimeout(() => {
      flash.textContent = "";
    }, 1500);
  });
  row.append(copyBtn, flash);
  return row;
}

// --- modal shell ------------------------------------------------------

let overlayEl = null;
// Every timer the currently-open modal owns; cleared as a group on close so
// no debounce/poll/countdown callback can ever fire (or touch removed DOM)
// after the modal is gone.
const timers = { debounce: null, poll: null, countdown: null };

function clearTimers() {
  if (timers.debounce) clearTimeout(timers.debounce);
  if (timers.poll) clearInterval(timers.poll);
  if (timers.countdown) clearInterval(timers.countdown);
  timers.debounce = null;
  timers.poll = null;
  timers.countdown = null;
}

function onKeydown(event) {
  if (event.key === "Escape") closeModal();
}

export function closeModal() {
  clearTimers();
  if (overlayEl) overlayEl.remove();
  overlayEl = null;
  document.removeEventListener("keydown", onKeydown);
}

/**
 * Opens the registration modal for one operator domain.
 * `domain` is remote data (from the operator's Nostr announcement) and is
 * only ever placed via textContent, never interpolated into markup.
 */
export function openRegisterModal({ origin, domain, ownerPubkey }) {
  closeModal();

  const overlay = document.createElement("div");
  overlay.className = "fixed inset-0 bg-gray-900/50 flex items-center justify-center p-4 z-50";
  overlay.addEventListener("click", (event) => {
    if (event.target === overlay) closeModal();
  });

  const card = document.createElement("div");
  card.className = "w-full max-w-lg rounded-lg bg-white p-6 shadow-lg space-y-4 max-h-[90vh] overflow-y-auto";
  overlay.append(card);

  const header = document.createElement("div");
  header.className = "flex items-center justify-between";
  const title = document.createElement("h2");
  title.className = "text-xl font-bold text-gray-900";
  title.textContent = `Register @${domain}`;
  const closeBtn = document.createElement("button");
  closeBtn.type = "button";
  closeBtn.className = "text-gray-400 hover:text-gray-600 text-2xl leading-none";
  closeBtn.setAttribute("aria-label", "Close");
  closeBtn.textContent = "×";
  closeBtn.addEventListener("click", () => closeModal());
  header.append(title, closeBtn);
  card.append(header);

  const body = document.createElement("div");
  body.className = "space-y-4";
  card.append(body);

  document.getElementById("modal-root").append(overlay);
  overlayEl = overlay;
  document.addEventListener("keydown", onKeydown);

  renderForm(body, { origin, domain, ownerPubkey });
}

// --- step 1: username + destination + quote ---------------------------

function renderForm(body, { origin, domain, ownerPubkey }) {
  body.replaceChildren();

  // Sequence number guarding against out-of-order quote responses, plus the
  // username each successful quote applies to, so a stale quote for an
  // older input value can never enable submit for the current one.
  let quoteSeq = 0;
  let lastQuote = null; // { username, price_msat }

  const usernameField = document.createElement("div");
  const usernameLabel = document.createElement("label");
  usernameLabel.className = LABEL_CLASS;
  usernameLabel.htmlFor = "reg-username";
  usernameLabel.textContent = "Username";
  const usernameInput = document.createElement("input");
  usernameInput.id = "reg-username";
  usernameInput.type = "text";
  usernameInput.required = true;
  usernameInput.autocomplete = "off";
  usernameInput.className = INPUT_CLASS;
  usernameField.append(usernameLabel, usernameInput);

  const quoteStatus = document.createElement("p");
  quoteStatus.className = "mt-1 text-sm text-gray-500";

  const destField = document.createElement("div");
  const destLabel = document.createElement("label");
  destLabel.className = LABEL_CLASS;
  destLabel.htmlFor = "reg-destination";
  destLabel.textContent = "LNURL or Lightning Address";
  const destInput = document.createElement("textarea");
  destInput.id = "reg-destination";
  destInput.required = true;
  destInput.rows = 3;
  destInput.className = `${INPUT_CLASS} resize-y`;
  destInput.style.wordBreak = "break-all";
  destField.append(destLabel, destInput);

  const submitBtn = document.createElement("button");
  submitBtn.type = "button";
  submitBtn.className = PRIMARY_BUTTON_CLASS;
  submitBtn.textContent = "Register";
  submitBtn.disabled = true;

  const formError = document.createElement("div");

  body.append(usernameField, quoteStatus, destField, submitBtn, formError);

  function setQuoteMessage(text, variant) {
    quoteStatus.textContent = text;
    if (variant === "error") {
      quoteStatus.className = "mt-1 text-sm text-red-700";
      quoteStatus.setAttribute("role", "alert");
    } else if (variant === "success") {
      quoteStatus.className = "mt-1 text-sm text-green-700";
      quoteStatus.removeAttribute("role");
    } else {
      quoteStatus.className = "mt-1 text-sm text-gray-500";
      quoteStatus.removeAttribute("role");
    }
  }

  function updateSubmitEnabled() {
    submitBtn.disabled = !(lastQuote && lastQuote.username === usernameInput.value.trim());
  }

  async function runQuote() {
    const username = usernameInput.value.trim();
    if (!username) {
      lastQuote = null;
      setQuoteMessage("", "idle");
      submitBtn.disabled = true;
      return;
    }
    const mySeq = ++quoteSeq;
    setQuoteMessage("Checking…", "idle");
    submitBtn.disabled = true;
    try {
      const result = await apiQuote(origin, domain, username);
      if (mySeq !== quoteSeq) return; // superseded by a newer keystroke
      lastQuote = { username, price_msat: result.price_msat };
      if (result.price_msat === 0) {
        setQuoteMessage("This name is free.", "success");
      } else {
        const sats = Math.ceil(result.price_msat / 1000).toLocaleString("en-US");
        setQuoteMessage(`Price: ${sats} sats`, "idle");
      }
      updateSubmitEnabled();
    } catch (err) {
      if (mySeq !== quoteSeq) return;
      lastQuote = null;
      setQuoteMessage(describeError(err.message), "error");
      submitBtn.disabled = true;
    }
  }

  usernameInput.addEventListener("input", () => {
    submitBtn.disabled = true;
    if (timers.debounce) clearTimeout(timers.debounce);
    timers.debounce = setTimeout(() => {
      timers.debounce = null;
      runQuote();
    }, 400);
  });

  submitBtn.addEventListener("click", async () => {
    formError.replaceChildren();
    const username = usernameInput.value.trim();
    const destination = destInput.value.trim();
    // Defense in depth: the button is only enabled when this holds, but a
    // click can still race a fresh debounce/quote, so re-check here too.
    if (!lastQuote || lastQuote.username !== username) return;
    if (!destination) {
      formError.replaceChildren(alertParagraph("Enter a destination LNURL or Lightning Address."));
      return;
    }
    submitBtn.disabled = true;
    const payload = { domain, username, destination, ...(ownerPubkey && { owner_pubkey: ownerPubkey }) };
    try {
      if (lastQuote.price_msat === 0) {
        const result = await apiRegisterFree(origin, payload);
        renderSuccess(body, result);
      } else {
        const result = await apiRegisterStart(origin, payload);
        renderInvoice(body, origin, result);
      }
    } catch (err) {
      formError.replaceChildren(alertParagraph(describeError(err.message)));
      updateSubmitEnabled();
    }
  });
}

// --- step 2a: free registration / paid completion ----------------------

/** Shared by the free-registration success path and a completed paid poll. */
function renderSuccess(body, { address, management_token, active }) {
  body.replaceChildren();

  const addressP = document.createElement("p");
  addressP.className = "text-sm text-gray-700";
  const addressLabel = document.createElement("span");
  addressLabel.className = "font-medium";
  addressLabel.textContent = "Address: ";
  const addressValue = document.createElement("span");
  addressValue.className = "font-mono";
  addressValue.textContent = address;
  addressP.append(addressLabel, addressValue);
  body.append(addressP);

  const tokenLabel = document.createElement("p");
  tokenLabel.className = "text-sm font-medium text-gray-700";
  tokenLabel.textContent = "Management token";
  body.append(tokenLabel);

  const pre = document.createElement("pre");
  pre.className = "bg-gray-100 rounded p-3 text-sm break-all select-all";
  pre.textContent = management_token;
  body.append(pre, copyRow(management_token));

  const warning = document.createElement("p");
  warning.className = "text-sm font-semibold text-red-700";
  warning.textContent = "Store this token now — it is shown once.";
  body.append(warning);

  if (active === false) {
    const note = document.createElement("p");
    note.className = "text-xs text-gray-500";
    note.textContent = "Waiting for a Nostr relay acknowledgement.";
    body.append(note);
  }
}

// --- step 2b: paid invoice + polling ------------------------------------

function renderInvoice(body, origin, { id, bolt11, amount_msat, expires_at }) {
  body.replaceChildren();

  const amountP = document.createElement("p");
  amountP.className = "text-base font-medium text-gray-900";
  amountP.textContent = `Pay ${Math.ceil(amount_msat / 1000).toLocaleString("en-US")} sats`;
  body.append(amountP);

  const countdownP = document.createElement("p");
  countdownP.className = "text-sm text-gray-500";
  body.append(countdownP);

  const qrContainer = document.createElement("div");
  qrContainer.className = "flex justify-center";
  // The one deliberate exception to "remote/user strings go through
  // textContent only": createSvgTag() returns a ready-made SVG *string*
  // rather than a DOM node, so injecting it requires innerHTML. This is
  // still safe because the QR is generated entirely client-side by the
  // qrcode-generator library from `bolt11`, which is our own operator's
  // fresh invoice string (just returned by our own registerStart call) —
  // there is no attacker-controlled markup anywhere in this string.
  const qr = qrcode(0, "L");
  qr.addData(bolt11.toUpperCase());
  qr.make();
  qrContainer.innerHTML = qr.createSvgTag({ cellSize: 4, margin: 4 });
  body.append(qrContainer);

  const invoiceLabel = document.createElement("p");
  invoiceLabel.className = "text-sm font-medium text-gray-700";
  invoiceLabel.textContent = "Invoice";
  body.append(invoiceLabel);

  const pre = document.createElement("pre");
  pre.className = "bg-gray-100 rounded p-3 text-sm break-all select-all";
  pre.textContent = bolt11;
  body.append(pre, copyRow(bolt11));

  let statusP = document.createElement("p");
  statusP.className = "text-sm text-gray-500";
  statusP.textContent = "Waiting for payment…";
  body.append(statusP);

  let stopped = false;

  function stop() {
    stopped = true;
    if (timers.poll) {
      clearInterval(timers.poll);
      timers.poll = null;
    }
    if (timers.countdown) {
      clearInterval(timers.countdown);
      timers.countdown = null;
    }
  }

  function showExpired() {
    stop();
    const alertEl = alertParagraph("This invoice has expired.");
    statusP.replaceWith(alertEl);
    statusP = alertEl;
  }

  function tickCountdown() {
    const remaining = expires_at - Math.floor(Date.now() / 1000);
    if (remaining <= 0) {
      countdownP.textContent = "Expired";
      showExpired();
      return;
    }
    const mm = Math.floor(remaining / 60).toString().padStart(2, "0");
    const ss = (remaining % 60).toString().padStart(2, "0");
    countdownP.textContent = `Expires in ${mm}:${ss}`;
  }

  async function poll() {
    if (stopped) return;
    try {
      const result = await apiRegisterStatus(origin, id);
      if (stopped) return;
      if (result.state === "pending_payment") {
        statusP.textContent = "Waiting for payment…";
      } else if (result.state === "publishing") {
        statusP.textContent = "Paid — waiting for relay acknowledgement…";
      } else if (result.state === "complete") {
        stop();
        renderSuccess(body, result);
      } else if (result.state === "expired") {
        showExpired();
      }
    } catch (err) {
      if (stopped) return;
      stop();
      const alertEl = alertParagraph(describeError(err.message));
      statusP.replaceWith(alertEl);
      statusP = alertEl;
    }
  }

  tickCountdown();
  timers.countdown = setInterval(tickCountdown, 1000);
  timers.poll = setInterval(poll, 3000);
}
