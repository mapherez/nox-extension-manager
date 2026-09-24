const STORAGE_KEY = "blockedDomains";

export async function getBlockedDomains() {
  const result = await chrome.storage.local.get(STORAGE_KEY);
  const entries = Array.isArray(result[STORAGE_KEY]) ? result[STORAGE_KEY] : [];

  return entries
    .filter((entry) => entry && typeof entry.domain === "string")
    .map((entry) => ({
      domain: entry.domain,
      purgeFuture: entry.purgeFuture !== false
    }));
}

export async function saveBlockedDomains(entries) {
  await chrome.storage.local.set({
    [STORAGE_KEY]: entries
  });
}

export async function upsertBlockedDomain(domain, purgeFuture) {
  const entries = await getBlockedDomains();
  const existingIndex = entries.findIndex((entry) => entry.domain === domain);

  if (existingIndex >= 0) {
    entries[existingIndex] = { domain, purgeFuture };
  } else {
    entries.push({ domain, purgeFuture });
  }

  entries.sort((a, b) => a.domain.localeCompare(b.domain));
  await saveBlockedDomains(entries);
  return entries;
}

export async function removeBlockedDomain(domain) {
  const entries = await getBlockedDomains();
  const nextEntries = entries.filter((entry) => entry.domain !== domain);
  await saveBlockedDomains(nextEntries);
  return nextEntries;
}
