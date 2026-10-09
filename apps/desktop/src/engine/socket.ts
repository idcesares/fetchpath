import { ON_THIS_PC } from "../ui/actions";
import type { EngineApi } from "./api";
import type { EngineConnection, JobDetails, JobDraft, JobSnapshot, QueueStats } from "./types";

/** A folder the engine lets a browser save into: the label is shown, the path is sent. */
export interface FolderChoice {
  label: string;
  path: string;
}

/** An engine refusal, as the views already show one: the message is what the person reads. */
export class EngineError extends Error {
  readonly code: string;
  constructor(code: string, message: string) {
    super(message);
    this.name = "EngineError";
    this.code = code;
  }
}

export type LinkState = "connecting" | "open" | "offline" | "refused";

export interface SocketEngine extends EngineApi {
  folderChoices(): Promise<FolderChoice[]>;
  /** What the link to the engine is doing now. */
  linkState(): LinkState;
  /** Opens the first connection. Call once, after the listeners are set. */
  start(): void;
}

interface Pending {
  resolve(value: unknown): void;
  reject(error: Error): void;
}

const FIRST_DELAY_MS = 500;
const MAX_DELAY_MS = 10_000;
const RELOAD_KEY = "fetchpath.web.reload";

/** The engine over `/ui/socket`: same origin, so the sign-in cookie rides along. */
export function createSocketEngine(): SocketEngine {
  let socket: WebSocket | null = null;
  let state: LinkState = "connecting";
  let nextId = 1;
  let delay = FIRST_DELAY_MS;
  let timer = 0;
  const pending = new Map<number, Pending>();
  const queueListeners = new Set<() => void>();
  const engineListeners = new Set<() => void>();

  const fire = (listeners: Set<() => void>): void => {
    for (const listener of listeners) listener();
  };

  function setState(next: LinkState): void {
    if (state === next) return;
    state = next;
    fire(engineListeners);
  }

  function failPending(message: string): void {
    const error = new EngineError("connection_lost", message);
    for (const call of pending.values()) call.reject(error);
    pending.clear();
  }

  /** Reloads once so the engine's signed-out page shows; again only after a socket has opened. */
  function reloadOnce(): boolean {
    try {
      if (sessionStorage.getItem(RELOAD_KEY) !== null) return false;
      sessionStorage.setItem(RELOAD_KEY, "1");
    } catch {
      // No session storage: a reload cannot be rate limited, so none is made.
      return false;
    }
    location.reload();
    return true;
  }

  async function afterClose(): Promise<void> {
    try {
      await fetch("/", { cache: "no-store" });
    } catch {
      // The engine cannot be reached: keep trying and say so.
      setState("offline");
      schedule();
      return;
    }
    // The engine answers but refused the socket: signed out or replaced.
    if (reloadOnce()) return;
    setState("refused");
    schedule();
  }

  function schedule(): void {
    window.clearTimeout(timer);
    timer = window.setTimeout(connect, delay);
    delay = Math.min(delay * 2, MAX_DELAY_MS);
  }

  function connect(): void {
    const scheme = location.protocol === "https:" ? "wss" : "ws";
    const next = new WebSocket(`${scheme}://${location.host}/ui/socket`);
    socket = next;
    next.addEventListener("open", () => {
      if (socket !== next) return;
      try {
        sessionStorage.removeItem(RELOAD_KEY);
      } catch {
        // Without session storage no reload was made, so there is nothing to clear.
      }
      delay = FIRST_DELAY_MS;
      state = "open";
      fire(engineListeners);
      fire(queueListeners);
    });
    next.addEventListener("message", (event) => {
      if (socket !== next || typeof event.data !== "string") return;
      let frame: {
        id?: number;
        ok?: unknown;
        error?: { code?: string; message?: string };
        notice?: string;
      };
      try {
        frame = JSON.parse(event.data);
      } catch {
        return;
      }
      if (frame.notice === "queue") fire(queueListeners);
      else if (frame.notice === "engine") fire(engineListeners);
      if (typeof frame.id !== "number") return;
      const call = pending.get(frame.id);
      if (!call) return;
      pending.delete(frame.id);
      if (frame.error) {
        call.reject(new EngineError(frame.error.code ?? "error", frame.error.message ?? "Fetchpath refused that."));
      } else {
        call.resolve(frame.ok);
      }
    });
    next.addEventListener("close", () => {
      if (socket !== next) return;
      socket = null;
      failPending("The connection to Fetchpath closed.");
      if (state === "open") setState("offline");
      void afterClose();
    });
  }

  function call<T>(name: string, args: Record<string, unknown> = {}): Promise<T> {
    if (!socket || socket.readyState !== WebSocket.OPEN) {
      return Promise.reject(new EngineError("connection_lost", "Fetchpath is not reachable right now."));
    }
    const id = nextId++;
    return new Promise<T>((resolve, reject) => {
      pending.set(id, { resolve: resolve as (value: unknown) => void, reject });
      socket?.send(JSON.stringify({ id, call: name, args }));
    });
  }

  const onThisPc = <T>(): Promise<T> => Promise.reject(new EngineError("not_here", ON_THIS_PC));

  function listen(listeners: Set<() => void>, callback: () => void): Promise<() => void> {
    listeners.add(callback);
    return Promise.resolve(() => void listeners.delete(callback));
  }

  return {
    start: connect,
    linkState: () => state,
    listDownloads: () => call<JobSnapshot[]>("listDownloads"),
    queueStats: () => call<QueueStats>("queueStats"),
    downloadDetails: (jobId) => call<JobDetails>("downloadDetails", { jobId }),
    pauseDownload: (jobId) => call<void>("pauseDownload", { jobId }),
    resumeDownload: (jobId) => call<void>("resumeDownload", { jobId }),
    cancelDownload: (jobId) => call<void>("cancelDownload", { jobId }),
    startNow: (jobId) => call<void>("startNow", { jobId }),
    // A browser retries as it stands; a new link, folder or checksum is the desktop's to give.
    retryDownload: (jobId, url, destination, checksum) =>
      url !== null || destination !== null || checksum !== undefined
        ? onThisPc<JobSnapshot>()
        : call<JobSnapshot>("retryDownload", { jobId }),
    removeDownload: (jobId) => call<void>("removeDownload", { jobId }),
    revealDownload: (jobId) => call<void>("revealDownload", { jobId }),
    startBatch: (drafts: JobDraft[]) => call<JobSnapshot[]>("startBatch", { drafts }),
    folderChoices: () => call<FolderChoice[]>("folderChoices"),
    engineConnection: async (): Promise<EngineConnection> => {
      if (state === "open") return call<EngineConnection>("engineConnection");
      return {
        connected: false,
        message:
          state === "refused"
            ? "This page is no longer signed in. Open Fetchpath on this PC to sign in again."
            : state === "offline"
              ? "Fetchpath on this PC is not reachable. Reconnecting…"
              : "Connecting to Fetchpath…",
      };
    },
    onQueueChanged: (callback) => listen(queueListeners, callback),
    onEngineChanged: (callback) => listen(engineListeners, callback),
  };
}
