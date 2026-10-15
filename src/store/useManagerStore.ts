import { create } from 'zustand';
import { getSnapshot, runExtensionAction, selectChromeProfile, syncExtensions } from '../services/managerApi';
import type { ActionOptions, ExtensionAction, ManagerSnapshot } from '../types';

type RetryOperation = { action: ExtensionAction; id: string; options?: ActionOptions } | { action: 'sync' };

type ManagerStore = {
  snapshot: ManagerSnapshot | null;
  loading: boolean;
  syncing: boolean;
  syncError: string | null;
  activeExtensionId: string | null;
  activeAction: ExtensionAction | null;
  notice: string | null;
  error: string | null;
  retryOperation: RetryOperation | null;
  initialize: () => Promise<void>;
  sync: () => Promise<void>;
  selectProfile: (id: string) => Promise<void>;
  runAction: (action: ExtensionAction, id: string, options?: ActionOptions) => Promise<void>;
  retry: () => Promise<void>;
  clearError: () => void;
  clearNotice: () => void;
};

function errorText(error: unknown) {
  return error instanceof Error ? error.message : String(error);
}

export const useManagerStore = create<ManagerStore>((set, get) => ({
  snapshot: null,
  loading: true,
  syncing: false,
  syncError: null,
  activeExtensionId: null,
  activeAction: null,
  notice: null,
  error: null,
  retryOperation: null,
  initialize: async () => {
    try {
      set({ snapshot: await getSnapshot() });
    } catch {
      // First run legitimately has no cache yet; the sync below provides the useful error.
    } finally {
      set({ loading: false });
    }
    await get().sync();
  },
  sync: async () => {
    if (get().syncing || get().activeExtensionId) return;
    set({ syncing: true, syncError: null });
    try {
      set({ snapshot: await syncExtensions(), syncing: false });
    } catch (error) {
      set({
        syncing: false,
        syncError: errorText(error),
        retryOperation: { action: 'sync' },
      });
    }
  },
  selectProfile: async (id) => {
    if (get().syncing || get().activeExtensionId) return;
    set({ syncing: true, error: null, retryOperation: null });
    try { set({ snapshot: await selectChromeProfile(id), syncing: false }); }
    catch (error) { set({ syncing: false, error: errorText(error) }); }
  },
  runAction: async (action, id, options) => {
    if (get().activeExtensionId || get().syncing) return;
    set({ activeExtensionId: id, activeAction: action, error: null, notice: null });
    try {
      const result = await runExtensionAction(action, id, options);
      set({
        activeExtensionId: null,
        activeAction: null,
        snapshot: result.snapshot,
        notice: result.message,
        retryOperation: null,
      });
    } catch (error) {
      set({
        activeExtensionId: null,
        activeAction: null,
        error: errorText(error),
        retryOperation: { action, id, options },
      });
    }
  },
  retry: async () => {
    const operation = get().retryOperation;
    set({ error: null });
    if (!operation) return;
    if (operation.action === 'sync') {
      await get().sync();
    } else {
      await get().runAction(operation.action, operation.id, operation.options);
    }
  },
  clearError: () => set({ error: null }),
  clearNotice: () => set({ notice: null }),
}));
