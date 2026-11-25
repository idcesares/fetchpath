import type { EngineConnection, JobDetails, JobDraft, JobSnapshot, QueueStats } from "./types";

/** The engine as the queue views use it: implemented for Tauri and the local web UI socket. */
export interface EngineApi {
  listDownloads(): Promise<JobSnapshot[]>;
  queueStats(): Promise<QueueStats>;
  downloadDetails(jobId: string): Promise<JobDetails>;
  pauseDownload(jobId: string): Promise<void>;
  resumeDownload(jobId: string): Promise<void>;
  cancelDownload(jobId: string): Promise<void>;
  startNow(jobId: string): Promise<void>;
  retryDownload(jobId: string, url: string | null, destination: string | null, checksum?: string): Promise<JobSnapshot>;
  removeDownload(jobId: string): Promise<void>;
  revealDownload(jobId: string): Promise<void>;
  startBatch(drafts: JobDraft[]): Promise<JobSnapshot[]>;
  engineConnection(): Promise<EngineConnection>;
  onQueueChanged(callback: () => void): Promise<() => void>;
  onEngineChanged(callback: () => void): Promise<() => void>;
}
