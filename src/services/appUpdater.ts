import { getVersion } from '@tauri-apps/api/app';
import { isTauri } from '@tauri-apps/api/core';
import { relaunch } from '@tauri-apps/plugin-process';
import { check, type DownloadEvent, type Update } from '@tauri-apps/plugin-updater';

export type AppUpdateInfo = {
  currentVersion: string;
  version: string;
  date?: string;
  update: Update;
};

export type UpdateProgress = {
  downloadedBytes: number;
  totalBytes: number | null;
  percent: number | null;
};

export async function checkForAppUpdate(): Promise<AppUpdateInfo | null> {
  if (!isTauri()) return null;
  const update = await check({ headers: { 'Cache-Control': 'no-cache' } });
  if (!update) return null;
  return {
    currentVersion: update.currentVersion || (await getVersion()),
    version: update.version,
    date: update.date,
    update,
  };
}

export async function installAppUpdate(
  update: Update,
  onProgress: (progress: UpdateProgress) => void,
) {
  let downloadedBytes = 0;
  let totalBytes: number | null = null;
  await update.downloadAndInstall((event: DownloadEvent) => {
    if (event.event === 'Started') {
      downloadedBytes = 0;
      totalBytes = event.data.contentLength ?? null;
    } else if (event.event === 'Progress') {
      downloadedBytes += event.data.chunkLength;
    } else if (event.event === 'Finished' && totalBytes !== null) {
      downloadedBytes = totalBytes;
    }
    onProgress({
      downloadedBytes,
      totalBytes,
      percent: totalBytes ? Math.min(100, Math.round((downloadedBytes / totalBytes) * 100)) : null,
    });
  });
  // On Windows the native updater exits the app and launches the installer.
  if (__NOX_PLATFORM__ !== 'windows') await relaunch();
}
