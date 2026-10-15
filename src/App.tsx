import { getVersion } from '@tauri-apps/api/app';
import { isTauri } from '@tauri-apps/api/core';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { openUrl } from '@tauri-apps/plugin-opener';
import {
  AlertTriangle, Check, CheckCircle2, Circle, Download, GitFork, Minus, Package,
  RefreshCw, RotateCcw, Search, Square, Trash2, X,
} from 'lucide-react';
import { useEffect, useMemo, useRef, useState } from 'react';
import './App.css';
import { localIconUrl } from './services/managerApi';
import { useAppUpdaterStore, type AppUpdaterStatus } from './store/useAppUpdaterStore';
import { useManagerStore } from './store/useManagerStore';
import type { ExtensionAction, ExtensionStatus, ExtensionView } from './types';

const statusCopy: Record<ExtensionStatus, string> = {
  downloading: 'Downloading',
  readyToInstall: 'Ready to install', installed: 'Installed', updateReady: 'Update ready',
  locallyModified: 'Locally modified', unavailable: 'Unavailable', error: 'Error',
};

function App() {
  const snapshot = useManagerStore((state) => state.snapshot);
  const loading = useManagerStore((state) => state.loading);
  const syncing = useManagerStore((state) => state.syncing);
  const syncError = useManagerStore((state) => state.syncError);
  const activeExtensionId = useManagerStore((state) => state.activeExtensionId);
  const error = useManagerStore((state) => state.error);
  const notice = useManagerStore((state) => state.notice);
  const retryOperation = useManagerStore((state) => state.retryOperation);
  const retryExtensionId = retryOperation && 'id' in retryOperation ? retryOperation.id : null;
  const initialize = useManagerStore((state) => state.initialize);
  const sync = useManagerStore((state) => state.sync);
  const runAction = useManagerStore((state) => state.runAction);
  const retry = useManagerStore((state) => state.retry);
  const clearError = useManagerStore((state) => state.clearError);
  const clearNotice = useManagerStore((state) => state.clearNotice);
  const checkAppUpdate = useAppUpdaterStore((state) => state.check);
  const [query, setQuery] = useState('');
  const [confirmTarget, setConfirmTarget] = useState<{ extension: ExtensionView; action: 'remove' | 'repair' } | null>(null);

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

  const act = (action: ExtensionAction, id: string) => {
    if (action === 'repair') {
      const extension = snapshot?.extensions.find((entry) => entry.id === id);
      if (extension) setConfirmTarget({ extension, action });
      return;
    }
    void runAction(action, id);
  };

  return (
    <div className={`app-shell platform-${__NOX_PLATFORM__}`}>
      {__NOX_PLATFORM__ === 'macos' ? null : <Titlebar />}
      <AppUpdateBanner />
      <main className="workspace">
        <header className="page-header">
          <div><p className="eyebrow">LOCAL CHROME LIBRARY</p><h1>Extensions</h1></div>
          <div className="sync-cluster">
            <button className="button button-secondary sync-button" onClick={() => void sync()} disabled={syncing || Boolean(activeExtensionId)}>
              <RefreshCw size={19} className={syncing ? 'spin' : undefined} />
              {syncing ? 'Syncing…' : 'Sync extensions'}
            </button>
            <span className={syncError ? 'sync-copy sync-copy-error' : 'sync-copy'}>
              {syncStatus(syncing, syncError, snapshot?.lastSyncedAt ?? null)}
            </span>
          </div>
        </header>
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
            <ExtensionCard key={extension.id} extension={extension} index={index}
              busy={activeExtensionId === extension.id} downloading={syncing}
              disabled={syncing || Boolean(activeExtensionId && activeExtensionId !== extension.id)}
              onAction={act} onRemove={() => setConfirmTarget({ extension, action: 'remove' })} />
          ))}
        </section>
      </main>
      <AppFooter />
      {confirmTarget ? <ConfirmDialog extension={confirmTarget.extension} action={confirmTarget.action} onCancel={() => setConfirmTarget(null)}
        onConfirm={() => { void runAction(confirmTarget.action, confirmTarget.extension.id); setConfirmTarget(null); }} /> : null}
      {error ? <ErrorDialog message={error} repositoryUrl={snapshot?.repositoryUrl}
        repairId={retryExtensionId
          && snapshot?.extensions.some((entry) => entry.id === retryExtensionId && entry.status === 'locallyModified')
          ? retryExtensionId : null}
        onRepair={(id) => { clearError(); act('repair', id); }}
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
  onAction: (action: ExtensionAction, id: string) => void; onRemove: () => void };

export function ExtensionCard({ extension, index, busy, downloading, disabled, onAction, onRemove }: CardProps) {
  const iconUrl = localIconUrl(extension.iconPath);
  const installed = Boolean(extension.installedVersion);
  const displayStatus: ExtensionStatus = downloading ? 'downloading' : extension.status;
  return <article className={`extension-card status-${displayStatus}`}>
    <div className={`extension-icon extension-icon-${index % 4}`}>
      {iconUrl ? <img src={iconUrl} alt="" /> : <Package size={35} />}
    </div>
    <div className="extension-copy"><h2>{extension.name}</h2>
      <p>{extension.description || 'Chrome extension'}</p><VersionLine extension={extension} />
      {extension.problem ? <span className="problem-copy">{extension.problem}</span> : null}
    </div>
    <div className={`status-pill pill-${displayStatus}`}><StatusIcon status={displayStatus} />{statusCopy[displayStatus]}</div>
    <div className="card-actions">
      {extension.status === 'locallyModified' ? <button className="button button-secondary"
        disabled={busy || disabled} aria-label={`Restore repository files for ${extension.name}`}
        onClick={() => onAction('repair', extension.id)}><RotateCcw size={18} /> Restore files</button> : null}
      {extension.status === 'readyToInstall' ? <button className="button button-primary" disabled={busy || disabled}
        onClick={() => onAction('install', extension.id)}>{busy ? <RefreshCw className="spin" size={18} /> : <Download size={18} />} Install</button> : null}
      {extension.status === 'updateReady' ? <button className="button button-primary" disabled={busy || disabled}
        onClick={() => onAction('update', extension.id)}>{busy ? <RefreshCw className="spin" size={18} /> : <RefreshCw size={18} />} Update</button> : null}
      {installed ? <button className="button button-danger" disabled={busy || disabled} onClick={onRemove}>
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
  if (status === 'locallyModified' || status === 'unavailable' || status === 'error') return <AlertTriangle size={16} />;
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

export function ConfirmDialog({ extension, action, onCancel, onConfirm }: { extension: ExtensionView; action: 'remove' | 'repair'; onCancel: () => void; onConfirm: () => void }) {
  const repair = action === 'repair';
  const cancelButton = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    cancelButton.current?.focus();
    const handleKey = (event: KeyboardEvent) => event.key === 'Escape' && onCancel();
    window.addEventListener('keydown', handleKey);
    return () => window.removeEventListener('keydown', handleKey);
  }, [onCancel]);
  return <div className="dialog-backdrop" role="presentation" onMouseDown={(event) => event.target === event.currentTarget && onCancel()}>
    <section className="dialog" role="alertdialog" aria-modal="true" aria-labelledby="remove-title">
      <div className="dialog-icon danger">{repair ? <RotateCcw size={22} /> : <Trash2 size={22} />}</div>
      <h2 id="remove-title">{repair ? `Restore repository files for ${extension.name}?` : `Remove ${extension.name} from Chrome?`}</h2>
      <p>{repair ? 'The repository version will replace this extension’s local files. A backup of the current files will be kept in .nox/backups. Only this extension will be reconnected in Chrome.' : 'The local files will stay in Documents, ready to install again.'}</p>
      <div className="dialog-actions"><button ref={cancelButton} className="button button-secondary" onClick={onCancel}>Cancel</button>
        <button className="button button-danger-solid" onClick={onConfirm}>{repair ? 'Restore files' : 'Remove'}</button></div>
    </section></div>;
}

function ErrorDialog({ message, repositoryUrl, repairId, onClose, onRetry, onRepair }: {
  message: string; repositoryUrl?: string; repairId: string | null; onClose: () => void;
  onRetry: () => void; onRepair: (id: string) => void;
}) {
  return (
    <div className="dialog-backdrop">
      <section
        className="dialog"
        role="alertdialog"
        aria-modal="true"
        aria-labelledby="error-title"
      >
        <div className="dialog-icon warning">
          <AlertTriangle size={23} />
        </div>
        <h2 id="error-title">NoX could not complete the action</h2>
        <p>{message}</p>
        <div className="dialog-actions dialog-actions-spread">
          <button
            className="button button-ghost"
            onClick={() =>
              void openUrl(
                repositoryUrl ??
                  "https://github.com/mapherez/nox-extension-manager",
              )
            }
          >
            <GitFork size={17} /> Open GitHub
          </button>
          <div>
            <button className="button button-secondary" onClick={onClose}>
              Close
            </button>
            {repairId ? (
              <button
                className="button button-secondary"
                onClick={() => onRepair(repairId)}
              >
                <RotateCcw size={17} /> Restore files
              </button>
            ) : null}
            <button className="button button-primary" onClick={onRetry}>
              Retry
            </button>
          </div>
        </div>
      </section>
    </div>
  );
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
