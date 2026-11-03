// Per-machine NoX Backend settings. All values stay in chrome.storage.local:
// the API key is a credential and must never ride on sync storage.

const SETTINGS_KEY = "noxBackend"; // { serverUrl, apiKey, clientName }
const CLIENT_ID_KEY = "noxClientId"; // stable per-device UUID, like NoX Sync's client identity
const SYNC_META_KEY = "noxSyncMeta"; // { lastKnownServerRevision }

export async function getBackendSettings() {
  const [settings, clientId] = await chrome.storage.local.get([SETTINGS_KEY, CLIENT_ID_KEY]);

  if (!clientId[CLIENT_ID_KEY]) {
    const id = crypto.randomUUID();
    await chrome.storage.local.set({ [CLIENT_ID_KEY]: id });
    return { ...settings[SETTINGS_KEY], clientId: id };
  }

  return { ...settings[SETTINGS_KEY], clientId: clientId[CLIENT_ID_KEY] };
}

export async function saveBackendSettings({ serverUrl, apiKey, clientName }) {
  const clean = {
    serverUrl: (serverUrl || "").trim().replace(/\/+$/, ""),
    apiKey: (apiKey || "").trim(),
    clientName: (clientName || "").trim() || "Chrome"
  };

  if (clean.serverUrl) {
    const url = new URL(clean.serverUrl);
    if (url.protocol !== "http:" && url.protocol !== "https:") {
      throw new Error("The backend URL must use http or https.");
    }
  }

  await chrome.storage.local.set({ [SETTINGS_KEY]: clean });
}

export async function getLastKnownRevision() {
  const meta = await chrome.storage.local.get(SYNC_META_KEY);
  const value = Number(meta[SYNC_META_KEY]?.lastKnownServerRevision);
  return Number.isFinite(value) ? value : 0;
}

export async function setLastKnownRevision(revision) {
  await chrome.storage.local.set({ [SYNC_META_KEY]: { lastKnownServerRevision: revision } });
}
