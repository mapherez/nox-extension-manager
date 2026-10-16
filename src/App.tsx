import { getVersion } from '@tauri-apps/api/app';
import { isTauri } from '@tauri-apps/api/core';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { openUrl } from '@tauri-apps/plugin-opener';
import {
  AlertTriangle, Check, CheckCircle2, Circle, Download, GitFork, Minus, Package,
  RefreshCw, Search, Square, Trash2, X,
} from 'lucide-react';
import { useEffect, useMemo, useRef, useState } from 'react';
import './App.css';
import { localIconUrl, prepareInstallation } from './services/managerApi';
import { useAppUpdaterStore, type AppUpdaterStatus } from './store/useAppUpdaterStore';
import { useManagerStore } from './store/useManagerStore';
import type { ActionOptions, ActionTarget, ChromeInstallation, ExtensionAction, ExtensionStatus, ExtensionView, OverwriteConsent } from './types';

const statusCopy: Record<ExtensionStatus, string> = {
  notInstalled: 'Not installed', ambiguous: 'Choose installation', detectionUnavailable: 'Detection unavailable',
  downloading: 'Downloading',
  installed: 'Installed', updateReady: 'Update ready',
  unavailable: 'Unavailable', error: 'Error',
};

function App() {
  const snapshot = useManagerStore((state) => state.snapshot);
  const loading = useManagerStore((state) => state.loading);
  const syncing = useManagerStore((state) => state.syncing);
  const syncError = useManagerStore((state) => state.syncError);
  const activeExtensionId = useManagerStore((state) => state.activeExtensionId);
  const activeAction = useManagerStore((state) => state.activeAction);
  const selectProfile = useManagerStore((state) => state.selectProfile);
  const error = useManagerStore((state) => state.error);
  const notice = useManagerStore((state) => state.notice);


  const initialize = useManagerStore((state) => state.initialize);
  const sync = useManagerStore((state) => state.sync);
  const runAction = useManagerStore((state) => state.runAction);
  const retry = useManagerStore((state) => state.retry);
  const clearError = useManagerStore((state) => state.clearError);
  const clearNotice = useManagerStore((state) => state.clearNotice);
  const checkAppUpdate = useAppUpdaterStore((state) => state.check);
  const [query, setQuery] = useState('');
  const [confirmTarget, setConfirmTarget] = useState<{
    extension: ExtensionView; action: 'remove' | 'install' | 'update'; options: ActionOptions;
  } | null>(null);
  const [preparing, setPreparing] = useState(false);

  useEffect(() => { void initialize(); void checkAppUpdate(true); }, [checkAppUpdate, initialize]);
  useEffect(() => {
    if (!notice) return;
    const timeout = window.setTimeout(clearNotice, 3600);
    return () => window.clearTimeout(timeout);
  }, [clearNotice, notice]);

  const filtered = useMemo(() => {
    const normalized = query.trim().toLowerCase();
    if (!normalized) return snapshot?.extensions ?? [];
    return (snapshot?.extensions ?? []).filter((extension) =>
      `${extension.name} ${extension.description} ${extension.id}`.toLowerCase().includes(normalized));
  }, [query, snapshot?.extensions]);

  const act = async (action: ExtensionAction, id: string, target?: ActionTarget) => {
    const extension = snapshot?.extensions.find((entry) => entry.id === id);
    if (!extension || preparing) return;
    if (action === 'remove') { setConfirmTarget({ extension, action, options: { target } }); return; }
    if (action === 'install' || action === 'update') {
      setPreparing(true);
      try {
        const consent = await prepareInstallation(id, target);
        if (consent) { setConfirmTarget({ extension, action, options: { target, consent } }); return; }
        await runAction(action, id, { target });
      } catch (error) {
        useManagerStore.setState({ error: error instanceof Error ? error.message : String(error), retryOperation: null });
      } finally { setPreparing(false); }
      return;
    }
    await runAction(action, id);
  };

  return (
    <div className={`app-shell platform-${__NOX_PLATFORM__}`}>
      {__NOX_PLATFORM__ === 'macos' ? null : <Titlebar />}
      <AppUpdateBanner />
      <main className="workspace">
        <header className="page-header">
          <div><p className="eyebrow">LOCAL CHROME LIBRARY</p><h1>Extensions</h1></div>
          <div className="sync-cluster">
            <label className="profile-picker">Chrome profile
              <select aria-label="Chrome profile" value={snapshot?.selectedProfile ?? 'Default'}
                disabled={syncing || preparing || Boolean(activeExtensionId)}
                onChange={(event) => void selectProfile(event.currentTarget.value)}>
                {(snapshot?.profiles?.length ? snapshot.profiles : [{ id: 'Default', name: 'Default' }]).map((profile) =>
                  <option key={profile.id} value={profile.id}>{profile.name} ({profile.id})</option>)}
              </select>
            </label>
            <button className="button button-secondary sync-button" onClick={() => void sync()} disabled={preparing || syncing || Boolean(activeExtensionId)}>
              <RefreshCw size={19} className={syncing ? 'spin' : undefined} />
              {syncing ? 'Syncing…' : 'Sync extensions'}
            </button>
            <span className={syncError ? 'sync-copy sync-copy-error' : 'sync-copy'}>
              {syncStatus(syncing, syncError, snapshot?.lastSyncedAt ?? null)}
            </span>
          </div>
        </header>
        {snapshot?.detectionError ? <p className="problem-copy" role="alert">Chrome detection failed: {snapshot.detectionError}</p> : null}
        <label className="search-box">
          <Search size={23} aria-hidden="true" />
          <input type="search" value={query} onChange={(event) => setQuery(event.currentTarget.value)}
            placeholder="Search extensions…" aria-label="Search extensions" />
        </label>
        <section className="extension-list" aria-busy={loading || syncing} aria-live="polite">
          {loading && !snapshot ? <LoadingCards /> : null}
          {!loading && filtered.length === 0 ? (
            <div className="empty-state"><Package size={34} />
              <strong>{query ? 'No matching extensions' : 'No extensions available yet'}</strong>
              <span>{query ? 'Try a different search.' : 'Sync the repository to look for extensions.'}</span>
            </div>) : null}
          {filtered.map((extension, index) => (
            <ExtensionCard key={`${snapshot?.selectedProfile}:${extension.id}`} extension={extension} index={index}
              busy={activeExtensionId === extension.id} downloading={activeExtensionId === extension.id && activeAction === 'download'}
              profileId={snapshot?.selectedProfile ?? 'Default'}
              disabled={preparing || syncing || Boolean(activeExtensionId && activeExtensionId !== extension.id)}
              onAction={(action, id, target) => void act(action, id, target)} onRemove={(target) => void act('remove', extension.id, target)} />
          ))}
        </section>
      </main>
      <AppFooter />
      {confirmTarget ? <ConfirmDialog extension={confirmTarget.extension} action={confirmTarget.action} target={confirmTarget.options.target} consent={confirmTarget.options.consent} onCancel={() => setConfirmTarget(null)}
        onConfirm={() => { void runAction(confirmTarget.action, confirmTarget.extension.id, confirmTarget.options); setConfirmTarget(null); }} /> : null}
      {error ? <ErrorDialog message={error} repositoryUrl={snapshot?.repositoryUrl}
        onClose={clearError} onRetry={() => void retry()} /> : null}
      {notice ? <div className="toast"><CheckCircle2 size={18} />{notice}</div> : null}
    </div>
  );
}

function Titlebar() {
  const appWindow = isTauri() ? getCurrentWindow() : null;
  return <header className="titlebar" data-tauri-drag-region onDoubleClick={(event) => {
    if (appWindow && !(event.target as HTMLElement).closest('button')) void appWindow.toggleMaximize();
  }}>
    <div className="brand" data-tauri-drag-region><img src="/assets/nox_logo.png" alt="" />
      <span data-tauri-drag-region>NoX Extension Manager</span></div>
    <div className="window-controls">
      <button aria-label="Minimize" disabled={!appWindow} onClick={() => void appWindow?.minimize()}><Minus size={18} /></button>
      <button aria-label="Maximize or restore" disabled={!appWindow} onClick={() => void appWindow?.toggleMaximize()}><Square size={15} /></button>
      <button className="close-control" aria-label="Close" disabled={!appWindow} onClick={() => void appWindow?.close()}><X size={20} /></button>
    </div>
  </header>;
}

type CardProps = { extension: ExtensionView; index: number; busy: boolean; downloading: boolean; disabled: boolean;
  profileId?: string; onAction: (action: ExtensionAction, id: string, target?: ActionTarget) => void;
  onRemove: (target?: ActionTarget) => void };

function versionAhead(remote: string, installed: string) {
  const left = remote.split('.').map(Number), right = installed.split('.').map(Number);
  for (let i = 0; i < 4; i++) { if ((left[i] ?? 0) !== (right[i] ?? 0)) return (left[i] ?? 0) > (right[i] ?? 0); }
  return false;
}

export function ExtensionCard({ extension, index, busy, downloading, disabled, profileId = 'Default', onAction, onRemove }: CardProps) {
  const [selectedId, setSelectedId] = useState('');
  const installations = extension.installations ?? [];
  const installation: ChromeInstallation | undefined = installations.length === 1 ? installations[0]
    : installations.find((item) => item.chromeExtensionId === selectedId);
  const target: ActionTarget | undefined = installation ? { profileId, chromeExtensionId: installation.chromeExtensionId,
    path: installation.path, version: installation.version } : undefined;
  const iconUrl = localIconUrl(extension.iconPath);
  const needsUpdate = installation && extension.availableVersion
    ? versionAhead(extension.availableVersion, installation.version) : extension.status === 'updateReady';
  const blocked = ['error', 'unavailable', 'detectionUnavailable'].includes(extension.status)
    || (installations.length > 1 && !installation) || Boolean(installation && !installation.unpacked);
  const actionable = !blocked && (!installation || needsUpdate);
  const downloaded = extension.downloadState === 'downloaded';
  const displayStatus: ExtensionStatus = downloading ? 'downloading'
    : extension.status === 'ambiguous' && installation ? (needsUpdate ? 'updateReady' : 'installed') : extension.status;
  const action: ExtensionAction = downloaded ? (installation ? 'update' : 'install') : 'download';
  return <article className={`extension-card status-${displayStatus}`} aria-busy={busy}>
    <div className={`extension-icon extension-icon-${index % 4}`}>
      {iconUrl ? <img src={iconUrl} alt="" /> : <Package size={35} />}
    </div>
    <div className="extension-copy"><h2>{extension.name}</h2>
      <p>{extension.description || 'Chrome extension'}</p>
      <VersionLine extension={{ ...extension, installedVersion: installation?.version ?? extension.installedVersion }} />
      {installations.length > 1 ? <label className="installation-picker">Chrome installation
        <select aria-label={`Installation for ${extension.name}`} value={selectedId} disabled={busy || disabled}
          onChange={(event) => setSelectedId(event.currentTarget.value)}>
          <option value="">Choose an installation</option>
          {installations.map((item) => <option key={item.chromeExtensionId} value={item.chromeExtensionId}>
            {item.chromeExtensionId} · v{item.version} · {item.path}
          </option>)}
        </select></label> : null}
      {installation ? <span className="installation-path" title={installation.path}>{installation.path}</span> : null}
      {installation && !installation.unpacked ? <span className="problem-copy">Managed by Chrome; use Chrome to change this installation.</span> : null}
      {extension.problem ? <span className="problem-copy">{extension.problem}</span> : null}
      {downloaded && actionable ? <span className="version-copy">Downloaded v{extension.downloadedVersion} · Ready to install</span> : null}
    </div>
    <div className={`status-pill pill-${displayStatus}`}><StatusIcon status={displayStatus} />{statusCopy[displayStatus]}</div>
    <div className="card-actions">
      {actionable ? <button className="button button-primary" disabled={busy || disabled}
        onClick={() => onAction(action, extension.id, target)}>
        {busy ? <RefreshCw className="spin" size={18} /> : <Download size={18} />}
        {busy ? (downloading ? 'Downloading…' : 'Installing…') : downloaded
          ? (installation ? 'Install update' : 'Install') : (installation ? 'Download update' : 'Download')}
      </button> : null}
      {installation ? <button className="button button-danger" disabled={busy || disabled || blocked} onClick={() => onRemove(target)}>
        <Trash2 size={18} /> Remove</button> : null}
    </div>
  </article>;
}

function VersionLine({ extension }: { extension: ExtensionView }) {
  if (extension.status === 'updateReady') return <span className="version-copy">
    Installed v{extension.installedVersion ?? extension.localVersion} <span>→</span> Available v{extension.availableVersion}</span>;
  if (extension.installedVersion) return <span className="version-copy">
    Installed v{extension.installedVersion} <span>·</span> Available v{extension.availableVersion ?? extension.localVersion ?? '—'}</span>;
  return <span className="version-copy">Available v{extension.availableVersion ?? extension.localVersion ?? '—'}</span>;
}

function StatusIcon({ status }: { status: ExtensionStatus }) {
  if (status === 'downloading') return <RefreshCw className="spin" size={16} />;
  if (status === 'installed') return <Check size={17} />;
  if (status === 'updateReady') return <RefreshCw size={16} />;
  if (status === 'unavailable' || status === 'error') return <AlertTriangle size={16} />;
  return <Circle size={16} />;
}

function AppUpdateBanner() {
  const status = useAppUpdaterStore((state) => state.status);
  const info = useAppUpdaterStore((state) => state.updateInfo);
  const progress = useAppUpdaterStore((state) => state.progress);
  const updaterError = useAppUpdaterStore((state) => state.error);
  const visible = useAppUpdaterStore((state) => state.bannerVisible);
  const dismiss = useAppUpdaterStore((state) => state.dismissBanner);
  const install = useAppUpdaterStore((state) => state.install);
  if (!visible || !info) return null;
  const installing = status === 'installing';
  return <aside className="update-banner" role="status">
    <div className="update-banner-icon"><Download size={21} /></div>
    <div><strong>NoX update v{info.version} is available</strong><span>Current version: {info.currentVersion}</span>
      {updaterError ? <span className="update-error">{updaterError}</span> : null}
      {installing ? <progress max={100} value={progress?.percent ?? undefined} /> : null}</div>
    <div className="update-banner-actions"><button className="button button-primary" disabled={installing} onClick={() => void install()}>
      {installing ? `Installing${progress?.percent != null ? ` ${progress.percent}%` : '…'}` : 'Install now'}</button>
      <button className="button button-ghost" disabled={installing} onClick={dismiss}>Later</button></div>
  </aside>;
}

function AppFooter() {
  const status = useAppUpdaterStore((state) => state.status);
  const info = useAppUpdaterStore((state) => state.updateInfo);
  const checkedAt = useAppUpdaterStore((state) => state.checkedAt);
  const check = useAppUpdaterStore((state) => state.check);
  const showBanner = useAppUpdaterStore((state) => state.showBanner);
  const [appVersion, setAppVersion] = useState('0.1.0');
  useEffect(() => {
    if (isTauri()) void getVersion().then(setAppVersion);
  }, []);
  const available = status === 'available' || status === 'installing';
  return <footer className="app-footer">
    <button className={available ? 'app-update-control update-available' : 'app-update-control'}
      onClick={() => available ? showBanner() : void check(true)} disabled={status === 'checking' || status === 'installing'}>
      <span className="footer-icon"><RefreshCw size={17} className={status === 'checking' ? 'spin' : undefined} /></span>
      <span>{appUpdateStatus(status, info?.version, checkedAt)}</span></button>
    <span className="footer-version">NoX Extension Manager · v{appVersion}</span>
  </footer>;
}

export function ConfirmDialog({ extension, action, target, consent, onCancel, onConfirm }: {
  extension: ExtensionView; action: 'remove' | 'install' | 'update'; target?: ActionTarget;
  consent?: OverwriteConsent; onCancel: () => void; onConfirm: () => void;
}) {
  const cancelButton = useRef<HTMLButtonElement>(null);
  const dialog = useRef<HTMLDialogElement>(null);
  const removing = action === 'remove';
  useEffect(() => { dialog.current?.showModal(); cancelButton.current?.focus(); }, []);
  return <dialog ref={dialog} className="dialog native-dialog" aria-labelledby="confirm-title"
    onCancel={(event) => { event.preventDefault(); onCancel(); }}>
    <div className="dialog-icon danger"><AlertTriangle size={22} /></div>
    <h2 id="confirm-title">{removing ? `Remove ${extension.name} from Chrome?` : `Replace files for ${extension.name}?`}</h2>
    <p>{removing ? 'Chrome will uninstall this exact extension and may delete its extension data. The extension files on disk will be kept.'
      : 'These files have no matching trusted baseline or contain local changes. They will be replaced only after a backup is created beside this folder.'}</p>
    <p className="confirmation-target">
      Profile: {target?.profileId ?? consent?.profileId ?? 'Default'}<br />
      {target ? <>Chrome ID: {target.chromeExtensionId}<br />Version: {target.version}<br /></> : null}
      Folder: {consent?.path ?? target?.path ?? extension.overwritePath}
    </p>
    <div className="dialog-actions"><button ref={cancelButton} className="button button-secondary" onClick={onCancel}>Cancel</button>
      <button className="button button-danger-solid" onClick={onConfirm}>{removing ? 'Remove' : 'Back up and install'}</button></div>
  </dialog>;
}

function ErrorDialog({ message, repositoryUrl, onClose, onRetry }: {
  message: string; repositoryUrl?: string; onClose: () => void; onRetry: () => void;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  useEffect(() => { dialog.current?.showModal(); }, []);
  return <dialog ref={dialog} className="dialog native-dialog" aria-labelledby="error-title"
    onCancel={(event) => { event.preventDefault(); onClose(); }}>
    <div className="dialog-icon warning"><AlertTriangle size={23} /></div>
    <h2 id="error-title">NoX could not complete the action</h2><p>{message}</p>
    <div className="dialog-actions dialog-actions-spread">
      <button className="button button-ghost" onClick={() => void openUrl(repositoryUrl ?? "https://github.com/mapherez/nox-extension-manager")}><GitFork size={17} /> Open GitHub</button>
      <div><button className="button button-secondary" onClick={onClose}>Close</button>
        <button className="button button-primary" onClick={onRetry}>Retry</button></div>
    </div>
  </dialog>;
}

function LoadingCards() { return <>{[0, 1, 2].map((item) => <div className="extension-card skeleton" key={item} />)}</>; }

export function syncStatus(syncing: boolean, error: string | null, date: string | null) {
  if (syncing) return 'Checking the GitHub repository';
  if (error) return 'Sync failed · click to retry';
  if (!date) return 'Not synced yet';
  const checked = new Date(date);
  if (Date.now() - checked.getTime() < 60_000) return 'Last synced just now';
  return `Last synced at ${checked.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}`;
}

export function appUpdateStatus(status: AppUpdaterStatus, version?: string, checkedAt?: string | null) {
  if (status === 'checking') return 'Checking for app updates…';
  if (status === 'installing') return `Installing app update${version ? ` v${version}` : ''}…`;
  if (status === 'available') return `App update v${version} available`;
  if (status === 'upToDate') { const time = checkedAt ? new Date(checkedAt).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }) : '';
    return `App is up to date${time ? ` · Checked at ${time}` : ''}`; }
  if (status === 'failed') return 'Update check failed · Retry';
  return 'Check for app updates';
}

export default App;
