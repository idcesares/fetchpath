import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { EngineApi } from "./api";
import type { EngineConnection, JobDetails, JobDraft, JobSnapshot, QueueStats } from "./types";

export const tauriEngine: EngineApi = {
  listDownloads: () => invoke<JobSnapshot[]>("list_downloads"),
  queueStats: () => invoke<QueueStats>("queue_stats"),
  downloadDetails: (jobId) => invoke<JobDetails>("download_details", { jobId }),
  pauseDownload: (jobId) => invoke("pause_download", { jobId }),
  resumeDownload: (jobId) => invoke("resume_download", { jobId }),
  cancelDownload: (jobId) => invoke("cancel_download", { jobId }),
  startNow: (jobId) => invoke("start_now", { jobId }),
  retryDownload: (jobId, url, destination, checksum) =>
    invoke<JobSnapshot>("retry_download", checksum === undefined ? { jobId, url, destination } : { jobId, url, destination, checksum }),
  removeDownload: (jobId) => invoke("remove_download", { jobId }),
  revealDownload: (jobId) => invoke("reveal_download", { jobId }),
  startBatch: (drafts: JobDraft[]) => invoke<JobSnapshot[]>("start_batch", { drafts }),
  engineConnection: () => invoke<EngineConnection>("engine_connection"),
  onQueueChanged: (callback) => listen("fetchpath://queue", () => callback()),
  onEngineChanged: (callback) => listen("fetchpath://engine", () => callback()),
};
