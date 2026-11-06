// NoX Backend client for PurgeUndesirables.
//
// Reads use the backend's stable client API (no session required).
// Writes go through the backend's /v1/sync/* protocol - the same protocol
// NoX Sync uses. This client implements no diff/merge logic: it keeps one
// state file in a dedicated vault and reacts to the plan the backend returns.

const VAULT_NAME = "purge-undesirables";
const STATE_PATH = "state.json";

export class BackendError extends Error {
  constructor(status, code, message) {
    super(message);
    this.name = "BackendError";
    this.status = status;
    this.code = code;
  }
}

export function friendlyError(error) {
  if (!(error instanceof BackendError)) {
    return error?.message || String(error);
  }

  switch (error.code) {
    case "AUTH_REQUIRED":
    case "AUTH_FAILED":
      return "The backend rejected the API key. Check it in the settings.";
    case "BAD_URL":
      return "The backend URL is not valid. Use e.g. https://backend.example.com";
    case "SERVER_UNREACHABLE":
      return "Cannot reach the backend. Check the URL and your network.";
    case "SYNC_LOCKED":
      return "The vault is locked by another sync. Try again in a moment.";
    case "SYNC_SESSION_STALE":
    case "SYNC_SESSION_NOT_FOUND":
      return "The sync session expired before it finished. Try again.";
    case "NOT_FOUND":
      return "The backend has no vault or state file for this extension yet.";
    case "PERMISSION_DENIED":
      return "Chrome did not grant permission to reach the backend. Press the button again.";
    case "BAD_STATE":
      return error.message;
    default:
      return `${error.code}: ${error.message}`;
  }
}

function baseServerUrl(serverUrl) {
  const trimmed = (serverUrl || "").trim().replace(/\/+$/, "");
  let url;
  try {
    url = new URL(trimmed);
  } catch {
    throw new BackendError(0, "BAD_URL", "The backend URL is not valid. Use e.g. https://backend.example.com");
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") {
    throw new BackendError(0, "BAD_URL", "The backend URL must use http or https.");
  }
  return trimmed;
}

async function ensureHostPermission(serverUrl) {
  const origin = new URL(baseServerUrl(serverUrl)).origin;
  const pattern = `${origin}/*`;

  const granted = await chrome.permissions.contains({ origins: [pattern] });
  if (!granted) {
    const approved = await chrome.permissions.request({ origins: [pattern] });
    if (!approved) {
      throw new BackendError(0, "PERMISSION_DENIED", "Chrome did not grant permission to reach the backend.");
    }
  }
}

async function request(serverUrl, apiKey, path, options = {}) {
  await ensureHostPermission(serverUrl);
  const url = `${baseServerUrl(serverUrl)}${path}`;

  let response;
  try {
    response = await fetch(url, {
      method: options.method || "GET",
      headers: {
        Authorization: `Bearer ${apiKey}`,
        ...(options.headers || {})
      },
      body: options.body
    });
  } catch {
    throw new BackendError(0, "SERVER_UNREACHABLE", "Cannot reach the backend. Check the URL and your network.");
  }

  if (!response.ok) {
    let code = "SERVER_ERROR";
    let message = `Backend returned HTTP ${response.status}.`;
    try {
      const body = await response.json();
      if (body && typeof body === "object") {
        if (typeof body.code === "string") code = body.code;
        if (typeof body.message === "string") message = body.message;
      }
    } catch {
      // Non-JSON error body; keep the default message.
    }
    throw new BackendError(response.status, code, message);
  }

  return response;
}

async function ensureVault(settings) {
  const response = await request(settings.serverUrl, settings.apiKey, "/v1/vaults");
  const listed = await response.json();
  const existing = (listed.vaults || []).find(
    (vault) => vault.name === VAULT_NAME && vault.status !== "DELETED"
  );
  if (existing) return existing;

  const created = await request(settings.serverUrl, settings.apiKey, "/v1/vaults", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ name: VAULT_NAME })
  });
  return created.json();
}

async function sha256Hex(bytes) {
  const digest = await crypto.subtle.digest("SHA-256", bytes);
  return [...new Uint8Array(digest)].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}

export async function testConnection(settings) {
  const response = await request(settings.serverUrl, settings.apiKey, "/v1/auth/check");
  return response.json(); // { ok, user, role }
}

/**
 * Push the local state to the backend vault.
 *
 * Returns one of:
 *   { action: "uploaded",            serverRevision }  state was committed
 *   { action: "already-up-to-date",  serverRevision }  remote hash matches
 *   { action: "remote-newer",        serverRevision }  backend has a newer copy
 *   { action: "conflict",            serverRevision }  both sides changed
 */
export async function saveState(settings, lastKnownRevision, envelope) {
  const vault = await ensureVault(settings);
  const bytes = new TextEncoder().encode(JSON.stringify(envelope));
  const hash = await sha256Hex(bytes);
  const size = bytes.byteLength;

  const beginResponse = await request(settings.serverUrl, settings.apiKey, "/v1/sync/begin", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      vaultId: vault.vaultId,
      clientId: settings.clientId,
      clientName: settings.clientName
    })
  });
  const { sessionId, serverRevision } = await beginResponse.json();

  const abort = () =>
    request(settings.serverUrl, settings.apiKey, "/v1/sync/abort", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ sessionId, clientId: settings.clientId, reason: "Aborted by client." })
    }).catch(() => {});

  try {
    const manifestResponse = await request(settings.serverUrl, settings.apiKey, "/v1/sync/manifest", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        sessionId,
        clientId: settings.clientId,
        vaultId: vault.vaultId,
        lastKnownServerRevision: lastKnownRevision,
        files: [{ path: STATE_PATH, hash, size, lastKnownRevision, deleted: false }],
        deletedPaths: []
      })
    });
    const plan = await manifestResponse.json();
    const action = (plan.actions || []).find((item) => item.path === STATE_PATH);

    if (action && action.type === "none") {
      await abort();
      return { action: "already-up-to-date", serverRevision };
    }

    if (action && action.type !== "upload") {
      // "download" (remote newer) or "conflict" / "delete_local" / "delete_remote".
      await abort();
      return {
        action: action.type === "download" ? "remote-newer" : "conflict",
        serverRevision
      };
    }

    await request(
      settings.serverUrl,
      settings.apiKey,
      `/v1/sync/upload/${encodeURIComponent(sessionId)}` +
        `?clientId=${encodeURIComponent(settings.clientId)}` +
        `&path=${encodeURIComponent(STATE_PATH)}` +
        `&hash=${hash}` +
        `&size=${size}`,
      { method: "PUT", body: bytes }
    );

    const commitResponse = await request(settings.serverUrl, settings.apiKey, "/v1/sync/commit", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ sessionId, clientId: settings.clientId })
    });
    const commitResult = await commitResponse.json();

    return { action: "uploaded", serverRevision: commitResult.serverRevision };
  } catch (error) {
    await abort();
    throw error;
  }
}

/**
 * Download the backend copy of the state file (stable read API, no session).
 * Returns { entries, savedAt, serverRevision }.
 */
export async function loadState(settings) {
  const vault = await ensureVault(settings);
  const response = await request(
    settings.serverUrl,
    settings.apiKey,
    `/v1/files/download?vaultId=${encodeURIComponent(vault.vaultId)}&path=${encodeURIComponent(STATE_PATH)}`
  );
  const text = await response.text();

  let parsed;
  try {
    parsed = JSON.parse(text);
  } catch {
    throw new BackendError(0, "BAD_STATE", "The remote state file is not valid JSON.");
  }
  if (!parsed || !Array.isArray(parsed.entries)) {
    throw new BackendError(0, "BAD_STATE", "The remote state file has an unexpected shape.");
  }

  return {
    entries: parsed.entries,
    savedAt: parsed.savedAt,
    serverRevision: Number(response.headers.get("X-NoX-Sync-Revision") || 0)
  };
}
