import { convertFileSrc } from '@tauri-apps/api/core';
import { invoke, isTauri } from '@tauri-apps/api/core';
import type { ActionOptions, ActionTarget, OverwriteConsent, ExtensionAction, ExtensionActionResult, ManagerSnapshot } from '../types';

const repositoryUrl = "https://github.com/mapherez/nox-extension-manager";

const browserPreview: ManagerSnapshot = {
  repositoryUrl,
  lastSyncedAt: new Date().toISOString(),
  extensions: [
    {
      id: 'better-bookmarks',
      name: 'Better Bookmarks',
      description: 'Bookmark manager for Chrome',
      localVersion: '1.4.0',
      installedVersion: '1.4.0',
      availableVersion: '1.5.0',
      iconPath: null,
      status: 'updateReady',
      problem: null,
    },
    {
      id: 'nox-tools',
      name: 'NoX Tools',
      description: 'Small utilities for Chrome',
      localVersion: '2.1.0',
      installedVersion: null,
      availableVersion: '2.1.0',
      iconPath: null,
      status: 'notInstalled',
      problem: null,
    },
    {
      id: 'tab-manager',
      name: 'Tab Manager',
      description: 'Organize your tabs and sessions',
      localVersion: '1.8.2',
      installedVersion: '1.8.2',
      availableVersion: '1.8.2',
      iconPath: null,
      status: 'installed',
      problem: null,
    },
  ],
};

export async function getSnapshot() {
  if (!isTauri()) return browserPreview;
  return invoke<ManagerSnapshot>('get_snapshot');
}

export async function syncExtensions() {
  if (!isTauri()) return browserPreview;
  return invoke<ManagerSnapshot>('sync_extensions');
}

export async function selectChromeProfile(id: string) {
  if (!isTauri()) return browserPreview;
  return invoke<ManagerSnapshot>('select_chrome_profile', { id });
}

export async function prepareInstallation(id: string, target?: ActionTarget) {
  if (!isTauri()) return null;
  return invoke<OverwriteConsent | null>('prepare_installation', { id, target: target ?? null });
}

export async function runExtensionAction(action: ExtensionAction, id: string, options: ActionOptions = {}) {
  if (!isTauri()) {
    return { message: `${action} preview completed`, snapshot: browserPreview };
  }
  return invoke<ExtensionActionResult>(`${action}_extension`, { id, target: options.target ?? null, consent: options.consent ?? null });
}

export function localIconUrl(path: string | null) {
  return path && isTauri() ? convertFileSrc(path) : null;
}
