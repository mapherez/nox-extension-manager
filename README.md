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
  pins downloads to the exact commit SHA returned by GitHub.
- Uses ETag/commit caching, verifies Git object hashes and records SHA-256
  snapshots of local files. Every sync reconciles the extension tree even
  after a `304 Not Modified`, so missing entries and failed downloads recover.
- Downloads new extensions immediately. Updates for registered Chrome
  extensions remain staged until the user clicks **Update**.
- Blocks changed content unless `manifest.json` contains a strictly newer
  version, and blocks updates when active local files were modified.
- Automates Install, Update, Remove and Repair through Windows UI Automation.
  Chrome's read-only profile registrations must associate the requested
  directory with the exact Chrome ID before an action can proceed. Controls
  must belong to that exact extension card; ambiguous controls are rejected.
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
downloads it again even if the repository commit has not changed. If NoX's
repository state was reset, existing directories are accepted only when all
their file hashes match the repository; differing files are left untouched.

For manual edits or an incorrect saved Chrome registration, choose
**Restore & repair** on that extension. After confirmation, NoX preserves the
current files under `.nox/backups/<extension-id>/before-repair-*`, restores the
repository files, and reconnects Chrome using the directory's actual
registration. It never removes another extension to repair a registration.

Deleting `.nox` also deletes installation records, integrity snapshots and
backups. Use Sync or Restore & repair for recovery instead. A failed ownership
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
To publish signed updates:

1. Store the matching private key contents in the repository secret
   `TAURI_SIGNING_PRIVATE_KEY`.
2. Store its password, if used, in
   `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`.
3. Run the **Release Tauri app** workflow from GitHub Actions.

The workflow builds signed Tauri updater artifacts, uploads the installers and
signatures with stable asset names, and generates `latest.json` using the actual
`browser_download_url` returned by GitHub. The update manifest is uploaded last,
after all artifact uploads have been confirmed. Dry-run and skip-upload modes
generate preview URLs only; they do not resolve published GitHub download URLs.
Authenticode signing is intentionally outside the pilot MVP.

## Verification scope

Automated tests cover repository integrity, atomic state storage, Chrome
automation targeting, updater recovery and the main interface states. Final UI
Automation verification still requires a real unlocked Windows 11 desktop with
Chrome Stable.
