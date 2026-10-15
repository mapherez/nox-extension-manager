# NoX Extension Manager

NoX Extension Manager is a Tauri 2 desktop app that synchronizes and manages
unpacked Google Chrome extensions from this repository. The first release
targets Windows 11 x64 and Chrome Stable's `Default` profile. The native
automation boundary is ready for later macOS AXUIElement and Linux AT-SPI
adapters.

## Current MVP

- Discovers immediate folders under `extensions/` that contain a Manifest V3
  `manifest.json`; there is no separate catalogue.
- Reads only `mapherez/nox-extension-manager`, currently from `master`, and
  lists `extensions/` directly through GitHub's contents API.
- Reads every extension's root `manifest.json` on each sync and compares its
  version. It does not request commit metadata or history. File hashes from
  directory listings verify downloads; SHA-256 snapshots protect local edits.
- Downloads new extensions immediately. Updates for registered Chrome
  extensions remain staged until the user clicks **Update**.
- Downloads an update when `manifest.json` contains a newer version, and
  blocks updates when active local files were modified. Changes published
  without increasing the manifest version are not treated as an update.
- Automates Install, Update, Remove and Repair through Windows UI Automation.
  Chrome's read-only profile registrations must associate the requested
  directory with the exact Chrome ID before an action can proceed. Controls
  used for removal must belong to that exact extension card. Installation and
  update load the exact folder through Load unpacked, and wait for Chrome's
  registration timestamp to change; they never search for a Reload button.
- Rebuilds installation records from Chrome's profile registrations and the
  local manifest on every snapshot and sync. A downloaded manifest alone does
  not count as installed unless Chrome actually registers that directory.
- Includes the app updater UI, signed updater artifacts and persistent footer
  status after dismissing the update banner.

No Chrome window is opened during startup synchronization. Chrome automation
only starts after an explicit Install, Update, Remove or Repair action.

## Storage

Runtime files are outside the application installation:

```text
Documents/_NoxChromeExtensions/
├── <extension-id>/
└── .nox/
    ├── backups/
    ├── logs/
    ├── staging/
    ├── updates/
    ├── chrome-state.json
    └── repository-state.json
```

Removing an extension removes it from Chrome but retains its downloaded files.
The application updater only clears the app's local WebView cache; it never
removes this Documents directory.

### Recovering extensions

Use **Sync extensions** after deleting an extension's download directory. NoX
downloads it again even if the manifest version has not changed. If NoX's
repository state was reset, existing directories are accepted only when all
their file hashes match the repository; differing files are left untouched.

For modified local files, choose **Restore files** on that extension. This
action is not shown on normally installed extensions. After confirmation, NoX
preserves the current files under `.nox/backups/<extension-id>/before-repair-*`, restores the
repository files, and reconnects Chrome using the directory's actual
registration. Stale NoX IDs are reconciled automatically from Chrome's profile.

Deleting `.nox` also deletes installation records, integrity snapshots and
backups. Use Sync or Restore files for recovery instead. A failed ownership
check blocks the action before any Remove or Reload is invoked.

## Development

Requirements:

- Node.js 24+
- Rust stable with the Windows MSVC toolchain
- Tauri's Windows prerequisites and WebView2
- Google Chrome Stable for end-to-end automation testing

```powershell
npm install
npm test
npm run build
cargo test --manifest-path src-tauri/Cargo.toml
npm run tauri:dev
```

The interface can also run in browser preview mode with `npm run dev`; it uses
demo card data and does not write to Documents.

## Publishing signed updates

The updater endpoint is fixed to:

```text
https://github.com/mapherez/nox-extension-manager/releases/latest/download/latest.json
```

The updater public key is already configured in `src-tauri/tauri.conf.json`.
App updates use the latest release's signed manifest, independently of extension
versions and repository history. **Install now** fetches that manifest again
before downloading, so retries use the current published URL.

To publish signed updates:

1. Store the matching private key contents in the repository secret
   `TAURI_SIGNING_PRIVATE_KEY`.
2. Store its password, if used, in
   `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`.
3. Run the **Release Tauri app** workflow from GitHub Actions.

The workflow builds signed Tauri updater artifacts, uploads the installers and
signatures with stable asset names, and generates `latest.json` using the actual
asset name returned by GitHub with the final release tag. Draft `untagged-*`
download URLs are never embedded in the manifest, since they expire on publish.
The update manifest is uploaded last, after all artifact uploads have been
confirmed. Published installer URLs are checked with HEAD before the manifest
is uploaded; drafts use final-tag URLs which become public on publication.
Dry-run and skip-upload modes generate preview URLs only; they do not resolve
published GitHub download URLs.
Authenticode signing is intentionally outside the pilot MVP.

## Verification scope

Automated tests cover repository integrity, atomic state storage, Chrome
automation targeting, updater recovery and the main interface states. Final UI
Automation verification still requires a real unlocked Windows 11 desktop with
Chrome Stable.
