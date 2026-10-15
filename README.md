# NoX Extension Manager

A Tauri 2 desktop app for unpacked Google Chrome extensions from this repository.

## Extension flow

- Startup checks app updates independently of the extension catalog.
- Sync lists remote `extensions/` and reads root manifests only. It does not
  inspect commits, fetch icons, download extension packages, or open Chrome.
- Select a Chrome profile (Default initially). Installed extensions come from
  Chrome's read-only profile registrations and their actual manifest files.
- Match the exact manifest `name`; there are no aliases or extension-specific
  exceptions. Compare numeric manifest versions. Same version means Installed;
  a newer remote version means Update available. No automatic downgrades.
- Duplicate names require selecting an installation. Failed profile/manifest
  reads show Detection unavailable, rather than pretending nothing is installed.
- Download / Download update fetches a verified package into staging. Install /
  Install update is a separate action and is never triggered by Sync.
- Updates use the existing folder and Chrome ID. A backup is retained beside
  the active folder (`.nox-backup-<folder>-<timestamp>`). Local edits or a missing
  integrity baseline require explicit confirmation of the folder being replaced.
- Installation/reload waits for Chrome to confirm loading the folder. Failed
  updates restore the previous files and attempt to reload the previous version.
- Remove confirms profile, actual ID, version and folder, then verifies the
  registration again before invoking the unique target control. Missing or
  ambiguous controls block the action. Other extensions are never fallbacks.
  Chrome uninstall may clear that extension's data; its source files are kept.
- Missing NoX state is recoverable from Chrome without Repair. A copied manifest
  alone does not count as installed.

## Storage

Catalog metadata, selected profile, logs and explicitly downloaded packages live
under `Documents/_NoxChromeExtensions/.nox/`. New installs use
`Documents/_NoxChromeExtensions/<catalog-folder>/`. Existing installations may
remain in another directory registered in Chrome.

Old catalog metadata and downloaded updates are retained and validated before use.
The old `chrome-state.json` is no longer an authority for installation detection.
Deleting NoX state does not uninstall Chrome extensions. Both the app's AppData
and extension data are preserved during app updates.

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

This refactor has not been exercised through automated tests or local builds.
Acceptance uses a real Chrome environment: fresh NoX state, two profiles,
explicit Download then Install, version comparison, rollback, and removal of only
the chosen extension.

Release publication first uploads every asset to a draft, then publishes it when
requested. A release-published workflow validates public updater links, including
drafts published manually. To repair only the manifest on an existing published
release, without building or creating a release:

```powershell
node .github/scripts/repair-updater-manifest.mjs
```

The repair command preserves installer signatures and verifies public URLs. It
uses GITHUB_TOKEN / GH_TOKEN or the existing Git credential manager.
