# PurgeUndesirables

A small Chrome Manifest V3 extension for selectively keeping chosen domains out of browsing history, reducing their chance of appearing as history-driven omnibox suggestions.

## What it does

- Add a domain such as `facebook.com` or `instagram.com`.
- Optionally purge existing matching URLs from Chrome history immediately.
- Optionally auto-purge future visits in the background.
- Matches the exact hostname and its subdomains only.
- `facebook.com` matches `www.facebook.com` and `m.facebook.com`.
- `facebook.com` does **not** match `notfacebook.com` or `facebook.com.evilsite.com`.

## What it does NOT touch

PurgeUndesirables only requests Chrome's `history` and `storage` permissions. It does not clear or request access to:

- cookies
- passwords
- cache
- logged-in sessions
- site storage
- bookmarks
- autofill

The source intentionally uses `chrome.history.deleteUrl()` only. It does not call `chrome.history.deleteAll()` or `chrome.history.deleteRange()`.

## Install locally

1. Open `chrome://extensions`.
2. Enable **Developer mode**.
3. Click **Load unpacked**.
4. Select the `PurgeUndesirables` folder.
5. Pin the extension from Chrome's extensions menu if you want the icon permanently visible in the toolbar.

## Usage

1. Click the toolbar icon.
2. Enter a root domain, for example `facebook.com`.
3. Leave **Purge existing history now** enabled if you want old matching history removed.
4. Leave **Auto-purge future visits** enabled to keep new visits from remaining in history.
5. Click **Add**.

You can later disable auto-purge for a domain, manually run **Purge now**, or remove the domain from the list.

Removing a domain from the list does not restore history that was already deleted.

## Syncing between computers (NoX Backend)

PurgeUndesirables can push and pull its domain list through a self-hosted
[NoX Backend](https://github.com/mapherez/nox-backend) instance, so the same
list can be used on multiple computers. Sync is fully manual:

- **Save data** pushes this device's domain list to the backend vault
  `purge-undesirables` (stored as `state.json`).
- **Load data** replaces this device's domain list with the backend copy
  (asks for confirmation first when the local list is not empty).

### Setup

1. Have a running NoX Backend instance and an API key from its dashboard.
2. In the extension popup, fill in **Backend URL** (base URL, e.g.
   `https://backend.example.com`), **API key** and a **Device name**.
3. Click **Save settings**, then **Test** (shows the authenticated user).
4. On first use Chrome asks for permission to reach the backend origin;
   approve it once per machine.
5. Click **Save data** on one computer (creates the vault on first run),
   then **Load data** on the others.

### Behavior

- `Save data` uses the backend's sync protocol and only uploads when this
  device's copy is the newest. If the backend copy is newer — or both sides
  changed — it tells you to press `Load data`.
- The backend URL, API key, device ID and last-seen backend revision are
  stored per machine in `chrome.storage.local`. The API key is only ever sent
  to the configured backend.
- Nothing syncs in the background; data leaves this machine only when you
  press a button.

## Notes

Chrome can source omnibox suggestions from places other than browsing history, including bookmarks and search suggestions. This extension specifically targets history-backed suggestions.

The initial purge queries Chrome history for matching text and then performs a second strict hostname check before deleting anything. The search is capped at 100,000 results per purge operation; if that cap is reached, the popup tells you to run **Purge now** again.
