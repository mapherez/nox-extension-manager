import { matchesDomain } from "./domain.js";
import { purgeDomainHistory } from "./history.js";
import { getBlockedDomains } from "./storage.js";

async function handleVisited(item) {
  if (!item.url) return;

  const entries = await getBlockedDomains();
  const blocked = entries.find(
    (entry) => entry.purgeFuture && matchesDomain(item.url, entry.domain)
  );

  if (!blocked) return;

  // deleteUrl removes this exact URL from history; it does not clear other history.
  await chrome.history.deleteUrl({ url: item.url });
}

chrome.history.onVisited.addListener((item) => {
  void handleVisited(item).catch((error) => {
    console.error("PurgeUndesirables: failed to purge visited URL", error);
  });
});

chrome.runtime.onMessage.addListener((message, _sender, sendResponse) => {
  if (message?.type !== "PURGE_DOMAIN" || typeof message.domain !== "string") {
    return false;
  }

  purgeDomainHistory(message.domain)
    .then((result) => sendResponse({ ok: true, result }))
    .catch((error) => sendResponse({ ok: false, error: error.message }));

  return true;
});
