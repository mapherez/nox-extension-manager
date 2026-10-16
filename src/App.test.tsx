import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { ConfirmDialog, ExtensionCard, appUpdateStatus, syncStatus } from './App';
import type { ActionTarget, ExtensionStatus, ExtensionView } from './types';

afterEach(cleanup);

const selectedTarget: ActionTarget = {
  profileId: 'Default', chromeExtensionId: 'abcdefghijklmnopabcdefghijklmnop',
  path: 'C:/extensions/sample', version: '1.0.0',
};

function extension(status: ExtensionStatus, downloaded = false): ExtensionView {
  const installed = ['installed', 'updateReady', 'unavailable', 'error'].includes(status);
  return {
    id: 'sample-extension', name: 'Sample Extension', description: 'Fixture extension',
    localVersion: null, installedVersion: installed ? '1.0.0' : null,
    availableVersion: status === 'updateReady' ? '1.1.0' : '1.0.0',
    iconPath: null, status, problem: status === 'error' ? 'Example failure' : null,
    downloadState: downloaded ? 'downloaded' : 'notDownloaded',
    downloadedVersion: downloaded ? (status === 'updateReady' ? '1.1.0' : '1.0.0') : null,
    installations: installed ? [{ ...selectedTarget, name: 'Sample Extension', unpacked: true }] : [],
  };
}

function renderCard(status: ExtensionStatus, downloaded = false, downloading = false) {
  const onAction = vi.fn();
  const onRemove = vi.fn();
  render(<ExtensionCard extension={extension(status, downloaded)} index={0} busy={downloading}
    downloading={downloading} disabled={false} onAction={onAction} onRemove={onRemove} />);
  return { onAction, onRemove };
}

describe('extension cards', () => {
  it.each([
    ['notInstalled', 'Not installed'],
    ['installed', 'Installed'],
    ['updateReady', 'Update ready'],
    ['ambiguous', 'Choose installation'],
    ['detectionUnavailable', 'Detection unavailable'],
    ['unavailable', 'Unavailable'],
    ['error', 'Error'],
  ] as const)('renders the %s state', (status, label) => {
    renderCard(status);
    expect(screen.getByText(label)).toBeInTheDocument();
  });

  it('shows Downloading only for an explicit active download', () => {
    renderCard('notInstalled', false, true);
    expect(screen.getByText('Downloading')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /downloading/i })).toBeDisabled();
  });

  it('offers Download before Install and does not request installation', () => {
    const { onAction } = renderCard('notInstalled');
    expect(screen.queryByRole('button', { name: /^install$/i })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: /^download$/i }));
    expect(onAction).toHaveBeenCalledWith('download', 'sample-extension', undefined);
    expect(onAction).toHaveBeenCalledOnce();
  });

  it('offers Install only after the package has been downloaded', () => {
    const { onAction } = renderCard('notInstalled', true);
    fireEvent.click(screen.getByRole('button', { name: /^install$/i }));
    expect(onAction).toHaveBeenCalledWith('install', 'sample-extension', undefined);
  });

  it('offers Download update with the exact current target and a separate Remove action', () => {
    const { onAction, onRemove } = renderCard('updateReady');
    expect(screen.getByText(/Installed v1.0.0/)).toBeInTheDocument();
    expect(screen.getByText(/Available v1.1.0/)).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /^install update$/i })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: /^download update$/i }));
    fireEvent.click(screen.getByRole('button', { name: /^remove$/i }));
    expect(onAction).toHaveBeenCalledWith('download', 'sample-extension', selectedTarget);
    expect(onRemove).toHaveBeenCalledWith(selectedTarget);
  });

  it('offers Install update after download, preserving the selected registration', () => {
    const { onAction } = renderCard('updateReady', true);
    fireEvent.click(screen.getByRole('button', { name: /^install update$/i }));
    expect(onAction).toHaveBeenCalledWith('update', 'sample-extension', selectedTarget);
  });

  it('requires an explicit selection when installed manifests share the same name', () => {
    const onAction = vi.fn();
    const onRemove = vi.fn();
    const first = extension('updateReady').installations![0];
    const second = { ...first, chromeExtensionId: 'ponmlkjihgfedcbaponmlkjihgfedcba', path: 'D:/other/sample' };
    render(<ExtensionCard extension={{ ...extension('ambiguous'), availableVersion: '1.1.0', installations: [first, second] }}
      index={0} busy={false} downloading={false} disabled={false} onAction={onAction} onRemove={onRemove} />);
    expect(screen.queryByRole('button', { name: /^remove$/i })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /^download update$/i })).not.toBeInTheDocument();
    fireEvent.change(screen.getByRole('combobox', { name: 'Installation for Sample Extension' }), {
      target: { value: second.chromeExtensionId },
    });
    fireEvent.click(screen.getByRole('button', { name: /^remove$/i }));
    expect(onRemove).toHaveBeenCalledWith({ ...selectedTarget, chromeExtensionId: second.chromeExtensionId, path: second.path });
  });

  it('does not offer Repair or Restore for a normally installed extension', () => {
    renderCard('installed');
    expect(screen.queryByRole('button', { name: /repair|restore/i })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /install|download/i })).not.toBeInTheDocument();
  });

  it('shows the exact removal target and waits for confirmation', () => {
    const onConfirm = vi.fn();
    const onCancel = vi.fn();
    render(<ConfirmDialog extension={extension('installed')} action="remove" target={selectedTarget}
      onConfirm={onConfirm} onCancel={onCancel} />);
    const dialog = screen.getByRole('dialog');
    expect(dialog).toHaveTextContent(selectedTarget.chromeExtensionId);
    expect(dialog).toHaveTextContent(selectedTarget.path);
    expect(dialog).toHaveTextContent('Profile: Default');
    expect(dialog).toHaveTextContent('may delete its extension data');
    expect(onConfirm).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(onCancel).toHaveBeenCalledOnce();
    expect(onConfirm).not.toHaveBeenCalled();
  });

  it('explains the replacement folder and backup before installing over untrusted files', () => {
    const onConfirm = vi.fn();
    render(<ConfirmDialog extension={extension('updateReady', true)} action="update" target={selectedTarget}
      consent={{ profileId: 'Default', path: selectedTarget.path, hashes: { 'content.js': 'original-hash' } }}
      onConfirm={onConfirm} onCancel={vi.fn()} />);
    const dialog = screen.getByRole('dialog');
    expect(dialog).toHaveTextContent('backup is created beside this folder');
    expect(dialog).toHaveTextContent(selectedTarget.path);
    expect(onConfirm).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: 'Back up and install' }));
    expect(onConfirm).toHaveBeenCalledOnce();
  });
});

describe('persistent status copy', () => {
  it('covers synchronization states', () => {
    expect(syncStatus(true, null, null)).toBe('Checking the GitHub repository');
    expect(syncStatus(false, 'offline', null)).toBe('Sync failed · click to retry');
    expect(syncStatus(false, null, new Date().toISOString())).toBe('Last synced just now');
  });

  it('keeps the app update visible after the banner is dismissed', () => {
    expect(appUpdateStatus('available', '1.2.0', null)).toBe('App update v1.2.0 available');
    expect(appUpdateStatus('checking')).toBe('Checking for app updates…');
    expect(appUpdateStatus('failed')).toBe('Update check failed · Retry');
  });
});
