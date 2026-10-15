import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { ConfirmDialog, ExtensionCard, appUpdateStatus, syncStatus } from './App';
import type { ExtensionStatus, ExtensionView } from './types';

afterEach(cleanup);

function extension(status: ExtensionStatus): ExtensionView {
  const installed = ['installed', 'updateReady', 'locallyModified', 'unavailable', 'error'].includes(status);
  return {
    id: 'test-extension',
    name: 'Test Extension',
    description: 'Used by the interface tests',
    localVersion: '1.0.0',
    installedVersion: installed ? '1.0.0' : null,
    availableVersion: status === 'updateReady' ? '1.1.0' : '1.0.0',
    iconPath: null,
    status,
    problem: status === 'error' ? 'Example failure' : null,
  };
}

function renderCard(status: ExtensionStatus, downloading = false) {
  const onAction = vi.fn();
  const onRemove = vi.fn();
  render(<ExtensionCard extension={extension(status)} index={0} busy={false}
    downloading={downloading} disabled={false} onAction={onAction} onRemove={onRemove} />);
  return { onAction, onRemove };
}

describe('extension cards', () => {
  it.each([
    ['readyToInstall', 'Ready to install'],
    ['installed', 'Installed'],
    ['updateReady', 'Update ready'],
    ['locallyModified', 'Locally modified'],
    ['unavailable', 'Unavailable'],
    ['error', 'Error'],
  ] as const)('renders the %s state', (status, label) => {
    renderCard(status);
    expect(screen.getByText(label)).toBeInTheDocument();
  });

  it('shows Downloading while a synchronization is active', () => {
    renderCard('installed', true);
    expect(screen.getByText('Downloading')).toBeInTheDocument();
  });

  it('invokes Install for an extension that is ready', () => {
    const { onAction } = renderCard('readyToInstall');
    fireEvent.click(screen.getByRole('button', { name: /install/i }));
    expect(onAction).toHaveBeenCalledWith('install', 'test-extension');
  });

  it('shows and invokes Update and Remove together', () => {
    const { onAction, onRemove } = renderCard('updateReady');
    expect(screen.getByText(/Installed v1.0.0/)).toBeInTheDocument();
    expect(screen.getByText(/Available v1.1.0/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: /update/i }));
    fireEvent.click(screen.getByRole('button', { name: /remove/i }));
    expect(onAction).toHaveBeenCalledWith('update', 'test-extension');
    expect(onRemove).toHaveBeenCalledOnce();
  });

  it('repairs the selected extension when local files were modified', () => {
    const { onAction, onRemove } = renderCard('locallyModified');
    fireEvent.click(screen.getByRole('button', { name: 'Restore repository files for Test Extension' }));
    expect(onAction).toHaveBeenCalledWith('repair', 'test-extension');
    expect(onRemove).not.toHaveBeenCalled();
  });

  it('explains replacement and backup before a repair can proceed', () => {
    const onConfirm = vi.fn();
    const onCancel = vi.fn();
    render(<ConfirmDialog extension={extension('locallyModified')} action="repair" onConfirm={onConfirm} onCancel={onCancel} />);
    expect(screen.getByRole('alertdialog')).toHaveTextContent('A backup of the current files will be kept');
    expect(onConfirm).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(onCancel).toHaveBeenCalledOnce();
    expect(onConfirm).not.toHaveBeenCalled();
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
