import { invoke } from "@tauri-apps/api/core";
import { required } from "./dom";
import { formatBytes } from "./format";

/** The content cache, as the engine reports it. */
export interface CacheView {
  bytes: number;
  entries: number;
  quota_bytes: number;
  min_quota_bytes: number;
  max_quota_bytes: number;
}

export interface CachePanelContext {
  showError(error: unknown): void;
  clearError(): void;
  setStatus(message: string): void;
  updateQuota(bytes: number, note: string): Promise<void>;
}

export interface CachePanel {
  refresh(): Promise<void>;
}

const GB = 1024 ** 3;

/** Content cache status, quota and clear controls inside Settings. */
export function createCachePanel(ctx: CachePanelContext): CachePanel {
  const cacheUsage = required<HTMLParagraphElement>("cache-usage");
  const cacheQuotaInput = required<HTMLInputElement>("setting-cache-quota");
  const cacheQuotaHint = required<HTMLParagraphElement>("setting-cache-quota-hint");
  const cacheClear = required<HTMLButtonElement>("cache-clear");

  function render(cache: CacheView): void {
    cacheUsage.textContent = cache.entries
      ? `${formatBytes(cache.bytes)} in ${cache.entries} ${cache.entries === 1 ? "file" : "files"}, of ${formatBytes(cache.quota_bytes)}.`
      : `Empty. It keeps up to ${formatBytes(cache.quota_bytes)}.`;
    cacheQuotaInput.min = String(cache.min_quota_bytes / GB);
    cacheQuotaInput.max = String(cache.max_quota_bytes / GB);
    cacheQuotaInput.value = String(Math.round((cache.quota_bytes / GB) * 100) / 100);
    cacheQuotaHint.textContent = `Between ${formatBytes(cache.min_quota_bytes)} and ${formatBytes(cache.max_quota_bytes)}. The oldest files go first when it is full.`;
    cacheClear.disabled = cache.entries === 0;
  }

  async function refresh(): Promise<void> {
    try {
      render(await invoke<CacheView>("cache_status"));
    } catch (error) {
      cacheUsage.textContent = "The cache cannot be read right now.";
      ctx.showError(error);
    }
  }

  cacheQuotaInput.addEventListener("change", async () => {
    const gigabytes = Number(cacheQuotaInput.value);
    if (!Number.isFinite(gigabytes) || gigabytes <= 0) {
      await refresh();
      ctx.showError("Enter a cache size in GB, such as 2.");
      return;
    }
    // The engine clamps the quota to its bounds; say so when it will.
    const min = Number(cacheQuotaInput.min) * GB;
    const max = Number(cacheQuotaInput.max) * GB;
    const requested = Math.round(gigabytes * GB);
    const kept = Math.min(Math.max(requested, min || requested), max || requested);
    await ctx.updateQuota(
      requested,
      kept === requested
        ? "Cache size updated."
        : `The cache keeps between ${formatBytes(min)} and ${formatBytes(max)}, so it was set to ${formatBytes(kept)}.`,
    );
    await refresh();
  });

  cacheClear.addEventListener("click", async () => {
    ctx.clearError();
    try {
      render(await invoke<CacheView>("clear_cache"));
      ctx.setStatus("Cache cleared. Your saved downloads are not affected.");
    } catch (error) {
      ctx.showError(error);
    }
  });

  return { refresh };
}
