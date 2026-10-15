import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { AppUpdateInfo, UpdateProgress } from '../services/appUpdater';

const updaterMocks = vi.hoisted(() => ({
  checkForAppUpdate: vi.fn(),
  installAppUpdate: vi.fn(),
}));

vi.mock('../services/appUpdater', () => updaterMocks);

import { useAppUpdaterStore } from './useAppUpdaterStore';

const updateInfo: AppUpdateInfo = {
  currentVersion: '1.0.0',
  version: '1.1.0',
  update: {} as AppUpdateInfo['update'],
};

describe('app updater banner persistence', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    updaterMocks.checkForAppUpdate.mockResolvedValue(null);
    updaterMocks.installAppUpdate.mockResolvedValue(undefined);
    useAppUpdaterStore.setState({
      status: 'idle',
      updateInfo: null,
      progress: null,
      checkedAt: null,
      bannerVisible: false,
      error: null,
    });
  });

  it('keeps the available update state when Later closes the banner', () => {
    useAppUpdaterStore.setState({ status: 'available', bannerVisible: true });
    useAppUpdaterStore.getState().dismissBanner();
    expect(useAppUpdaterStore.getState().status).toBe('available');
    expect(useAppUpdaterStore.getState().bannerVisible).toBe(false);
  });

  it('reopens a dismissed banner from the footer control', () => {
    useAppUpdaterStore.setState({ status: 'available', bannerVisible: false });
    useAppUpdaterStore.getState().showBanner();
    expect(useAppUpdaterStore.getState().bannerVisible).toBe(true);
  });

  it('shares one updater request between simultaneous checks', async () => {
    let finishCheck!: (result: AppUpdateInfo | null) => void;
    updaterMocks.checkForAppUpdate.mockReturnValue(
      new Promise<AppUpdateInfo | null>((resolve) => {
        finishCheck = resolve;
      }),
    );

    const firstCheck = useAppUpdaterStore.getState().check();
    const secondCheck = useAppUpdaterStore.getState().check();

    expect(updaterMocks.checkForAppUpdate).toHaveBeenCalledOnce();
    expect(useAppUpdaterStore.getState().status).toBe('checking');

    finishCheck(updateInfo);
    await Promise.all([firstCheck, secondCheck]);

    expect(useAppUpdaterStore.getState()).toMatchObject({
      status: 'available',
      updateInfo,
      bannerVisible: true,
    });
  });

  it('returns to an actionable state after installation fails', async () => {
    updaterMocks.checkForAppUpdate.mockResolvedValue(updateInfo);
    const partialProgress: UpdateProgress = {
      downloadedBytes: 64,
      totalBytes: 128,
      percent: 50,
    };
    updaterMocks.installAppUpdate.mockImplementationOnce(async (_update, onProgress) => {
      onProgress(partialProgress);
      throw new Error('Installation failed');
    });
    useAppUpdaterStore.setState({
      status: 'available',
      updateInfo,
      bannerVisible: true,
    });

    await useAppUpdaterStore.getState().install();

    expect(useAppUpdaterStore.getState()).toMatchObject({
      status: 'available',
      progress: null,
      error: 'Installation failed',
      bannerVisible: true,
    });

    await useAppUpdaterStore.getState().install();
    expect(updaterMocks.installAppUpdate).toHaveBeenCalledTimes(2);
  });

  it('clears a check error and can check again', async () => {
    updaterMocks.checkForAppUpdate.mockRejectedValueOnce(new Error('Network unavailable'));

    await useAppUpdaterStore.getState().check();
    expect(useAppUpdaterStore.getState()).toMatchObject({
      status: 'failed',
      error: 'Network unavailable',
    });

    await useAppUpdaterStore.getState().check();
    expect(updaterMocks.checkForAppUpdate).toHaveBeenCalledTimes(2);
    expect(useAppUpdaterStore.getState()).toMatchObject({
      status: 'upToDate',
      updateInfo: null,
      error: null,
    });
  });
});
