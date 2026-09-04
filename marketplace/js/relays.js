import { DEFAULT_RELAYS } from "./config.js";

export function currentRelays() {
  const param = new URLSearchParams(location.search).get("relays");
  if (!param) return [...DEFAULT_RELAYS];
  const relays = param.split(",").map(r => r.trim()).filter(r => r.startsWith("wss://"));
  return relays.length ? relays : [...DEFAULT_RELAYS];
}

export function setRelays(relays) {
  const url = new URL(location.href);
  url.searchParams.set("relays", relays.join(","));
  history.replaceState(null, "", url);
}

export function renderRelayEditor(container, onChange) {
  container.replaceChildren();
  for (const relay of currentRelays()) {
    const chip = document.createElement("span");
    chip.className = "inline-flex items-center gap-1 rounded-full bg-gray-100 px-3 py-1 text-xs font-mono";
    chip.textContent = relay;
    const remove = document.createElement("button");
    remove.textContent = "×";
    remove.className = "text-gray-500 hover:text-red-600";
    remove.onclick = () => { setRelays(currentRelays().filter(r => r !== relay)); onChange(); };
    chip.append(remove);
    container.append(chip);
  }
  const input = document.createElement("input");
  input.placeholder = "wss://…";
  input.className = "rounded-lg border border-gray-300 bg-gray-50 p-1.5 text-xs font-mono";
  input.onkeydown = (e) => {
    if (e.key !== "Enter") return;
    const value = input.value.trim();
    if (!value.startsWith("wss://")) return;
    const relays = currentRelays();
    if (relays.includes(value)) return;
    setRelays([...relays, value]);
    onChange();
  };
  container.append(input);
}
