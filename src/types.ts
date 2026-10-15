export type ExtensionStatus =
  | 'notInstalled'
  | 'ambiguous'
  | 'detectionUnavailable'
  | 'downloading'
  | 'readyToInstall'
  | 'installed'
  | 'updateReady'
  | 'locallyModified'
  | 'unavailable'
  | 'error';

export type ExtensionView = {
  id: string;
  name: string;
  description: string;
  localVersion: string | null;
  installedVersion: string | null;
  availableVersion: string | null;
  iconPath: string | null;
  status: ExtensionStatus;
  problem: string | null;
  downloadState?: 'notDownloaded' | 'downloaded';
  downloadedVersion?: string | null;
  installations?: ChromeInstallation[];
  requiresOverwriteConfirmation?: boolean;
  overwritePath?: string | null;
};

export type ManagerSnapshot = {
  extensions: ExtensionView[];
  lastSyncedAt: string | null;
  repositoryUrl: string;
  profiles?: ChromeProfile[];
  selectedProfile?: string;
  detectionError?: string | null;
};

export type ExtensionActionResult = {
  message: string;
  snapshot: ManagerSnapshot;
};

export type ExtensionAction = 'download' | 'install' | 'update' | 'remove' | 'repair';

export type ChromeProfile = { id: string; name: string; path: string };
export type ChromeInstallation = { chromeExtensionId: string; path: string; name: string; version: string; unpacked: boolean };
export type ActionTarget = { profileId: string; chromeExtensionId: string; path: string; version: string };
export type OverwriteConsent = { profileId: string; path: string; hashes: Record<string, string> };
export type ActionOptions = { target?: ActionTarget; consent?: OverwriteConsent };
