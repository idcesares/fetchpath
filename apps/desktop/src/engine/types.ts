export type JobState =
  | "scheduled"
  | "queued"
  | "running"
  | "paused"
  | "cancelling"
  | "completed"
  | "cancelled"
  | "failed"
  | "needs_source"
  | "awaiting_approval";

export type ApprovalReason = "outside_granted_folders" | "size_limit" | "rate_limit" | "peer_discovery" | "peer_upload" | "unknown";

export interface JobSnapshot {
  jobId: string;
  source: string;
  state: JobState;
  /** Queued until its drive has room above the disk reserve. */
  waitingForSpace?: boolean;
  bytesReceived: number;
  /** Absent whenever the source never stated a length. Never guessed. */
  totalBytes: number | null;
  bytesPerSecond?: number | null;
  etaSeconds?: number | null;
  attempt: number;
  destination: string | null;
  observedSha256: string | null;
  /** The SHA-256 the person supplied. Nothing that differs from it is saved. */
  expectedSha256?: string | null;
  cleanupPending: boolean;
  error: string | null;
  action:
    | "retry"
    | "choose_new_path"
    | "edit_link"
    | "recapture"
    | "refresh_source"
    | "configure_media_tools"
    | "check_checksum"
    | null;
  retryable: boolean;
  createdAtMs: number;
  notBeforeMs: number | null;
  finishedAtMs: number | null;
  kind: "file" | "media" | "torrent";
  qualityLabel: string | null;
  /** The agent that asked for it, when an agent did. */
  agent: string | null;
  /** Why it waits for the person's approval. */
  approvalReasons: ApprovalReason[];
  /** Finished by copying verified bytes from the cache, not by a transfer. */
  reusedFromCache?: boolean;
  /** Fingerprint of the paired computer it came from. */
  fromPairedDevice?: string | null;
}

export interface JobDraft {
  url: string;
  destination: string;
  notBeforeMs: number | null;
  checksum: string | null;
}

export interface QueueStats {
  running: number;
  queued: number;
  scheduled: number;
  paused: number;
  completed: number;
  failed: number;
  activeBytes: number;
  completedBytes: number;
  combinedBytesPerSecond: number;
  maxActiveDownloads: number;
}

/** One byte range in flight: received into memory, not yet written. */
export interface SegmentView {
  start: number;
  /** Inclusive. */
  end: number;
  received: number;
}

export interface JobDetails {
  job: JobSnapshot;
  segments: SegmentView[];
}

export interface EngineConnection {
  connected: boolean;
  /** Stopped on purpose, or would not start: the window waits for the person. */
  stopped?: boolean;
  message?: string;
  /** Connected to a queue saved by a newer Fetchpath: shown, never changed. */
  readOnly?: string;
}
