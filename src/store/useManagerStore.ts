import { create } from 'zustand';
import { getSnapshot, runExtensionAction, syncExtensions } from '../services/managerApi';
import type { ExtensionAction, ManagerSnapshot } from '../types';

type RetryOperation = { action: ExtensionAction; id: string } | { action: 'sync' };

type ManagerStore = {
  snapshot: ManagerSnapshot | null;
  loading: boolean;
  syncing: boolean;
  syncError: string | null;
  activeExtensionId: string | null;
  notice: string | null;
  error: string | null;
  retryOperation: RetryOperation | null;
  initialize: () => Promise<void>;
  sync: () => Promise<void>;
  runAction: (action: ExtensionAction, id: string) => Promise<void>;
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
  runAction: async (action, id) => {
    if (get().activeExtensionId || get().syncing) return;
    set({ activeExtensionId: id, error: null, notice: null });
    try {
      const result = await runExtensionAction(action, id);
      set({
        activeExtensionId: null,
        snapshot: result.snapshot,
        notice: result.message,
        retryOperation: null,
      });
    } catch (error) {
      set({
        activeExtensionId: null,
        error: errorText(error),
        retryOperation: { action, id },
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
      await get().runAction(operation.action, operation.id);
    }
  },
  clearError: () => set({ error: null }),
  clearNotice: () => set({ notice: null }),
}));
