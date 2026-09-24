import { normalizeDomain } from "./domain.js";
import {
  getBlockedDomains,
  removeBlockedDomain,
  saveBlockedDomains,
  upsertBlockedDomain
} from "./storage.js";

const form = document.querySelector("#add-form");
const domainInput = document.querySelector("#domain-input");
const purgeExistingInput = document.querySelector("#purge-existing");
const purgeFutureInput = document.querySelector("#purge-future");
const domainList = document.querySelector("#domain-list");
const emptyState = document.querySelector("#empty-state");
const domainCount = document.querySelector("#domain-count");
const status = document.querySelector("#status");
const submitButton = form.querySelector("button[type='submit']");

function setStatus(message = "", isError = false) {
  status.textContent = message;
  status.classList.toggle("error", isError);
}

async function purgeDomain(domain) {
  const response = await chrome.runtime.sendMessage({
    type: "PURGE_DOMAIN",
    domain
  });

  if (!response?.ok) {
    throw new Error(response?.error || "Could not purge history.");
  }

  return response.result;
}

function createDomainRow(entry) {
  const row = document.createElement("div");
  row.className = "domain-row";
  row.dataset.domain = entry.domain;

  const main = document.createElement("div");
  main.className = "domain-main";

  const toggle = document.createElement("label");
  toggle.className = "toggle";
  toggle.title = "Auto-purge future visits";

  const checkbox = document.createElement("input");
  checkbox.type = "checkbox";
  checkbox.checked = entry.purgeFuture;
  checkbox.setAttribute("aria-label", `Auto-purge future visits for ${entry.domain}`);

  const track = document.createElement("span");
  track.className = "toggle-track";

  toggle.append(checkbox, track);

  const text = document.createElement("div");
  const name = document.createElement("div");
  name.className = "domain-name";
  name.textContent = entry.domain;

  const autoLabel = document.createElement("div");
  autoLabel.className = "auto-label";
  autoLabel.textContent = entry.purgeFuture ? "Auto-purge on" : "Auto-purge off";

  text.append(name, autoLabel);
  main.append(toggle, text);

  const actions = document.createElement("div");
  actions.className = "domain-actions";

  const purgeButton = document.createElement("button");
  purgeButton.type = "button";
  purgeButton.className = "secondary";
  purgeButton.textContent = "Purge now";

  const removeButton = document.createElement("button");
  removeButton.type = "button";
  removeButton.className = "icon-button";
  removeButton.textContent = "×";
  removeButton.title = `Remove ${entry.domain} from the list`;
  removeButton.setAttribute("aria-label", `Remove ${entry.domain}`);

  checkbox.addEventListener("change", async () => {
    const entries = await getBlockedDomains();
    const target = entries.find((item) => item.domain === entry.domain);
    if (!target) return;

    target.purgeFuture = checkbox.checked;
    await saveBlockedDomains(entries);
    autoLabel.textContent = checkbox.checked ? "Auto-purge on" : "Auto-purge off";
    setStatus(
      checkbox.checked
        ? `${entry.domain}: future visits will be purged.`
        : `${entry.domain}: future visits will remain in history.`
    );
  });

  purgeButton.addEventListener("click", async () => {
    purgeButton.disabled = true;
    setStatus(`Purging ${entry.domain}…`);

    try {
      const result = await purgeDomain(entry.domain);
      const warning = result.searchLimitReached ? " Search limit reached; run Purge now again." : "";
      setStatus(`Removed ${result.deletedUrls} matching URL${result.deletedUrls === 1 ? "" : "s"}.${warning}`);
    } catch (error) {
      setStatus(error.message, true);
    } finally {
      purgeButton.disabled = false;
    }
  });

  removeButton.addEventListener("click", async () => {
    await removeBlockedDomain(entry.domain);
    setStatus(`${entry.domain} removed from the list.`);
    await renderDomains();
  });

  actions.append(purgeButton, removeButton);
  row.append(main, actions);
  return row;
}

async function renderDomains() {
  const entries = await getBlockedDomains();
  domainList.replaceChildren(...entries.map(createDomainRow));
  domainCount.textContent = String(entries.length);
  emptyState.hidden = entries.length > 0;
}

form.addEventListener("submit", async (event) => {
  event.preventDefault();
  setStatus();
  submitButton.disabled = true;

  try {
    const domain = normalizeDomain(domainInput.value);
    const purgeExisting = purgeExistingInput.checked;
    const purgeFuture = purgeFutureInput.checked;

    if (!purgeExisting && !purgeFuture) {
      throw new Error("Enable at least one purge option.");
    }

    await upsertBlockedDomain(domain, purgeFuture);
    await renderDomains();

    if (purgeExisting) {
      setStatus(`Purging existing ${domain} history…`);
      const result = await purgeDomain(domain);
      const warning = result.searchLimitReached ? " Search limit reached; use Purge now again." : "";
      setStatus(`Added ${domain}. Removed ${result.deletedUrls} matching URL${result.deletedUrls === 1 ? "" : "s"}.${warning}`);
    } else {
      setStatus(`Added ${domain}.`);
    }

    domainInput.value = "";
    domainInput.focus();
  } catch (error) {
    setStatus(error.message, true);
  } finally {
    submitButton.disabled = false;
  }
});

await renderDomains();
domainInput.focus();
