import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { createAgentsPanel } from "./agents";
import { createCachePanel } from "./cache";
import { type DialogManager, errorMessage } from "./dialogs";
import { required } from "./dom";
import { formatBytes } from "./format";
import { createLanPanel } from "./lan";
import { createRulesPanel } from "./rules";
import { createSharePanel } from "./share";

export interface Settings {
  maxActiveDownloads: number;
  defaultDestinationDir: string | null;
  autoRetry: boolean;
  autoRetryMaxAttempts: number;
  autoRetryBaseDelaySeconds: number;
  closeToTray: boolean;
  powerMode: boolean;
  mediaToolsDir: string | null;
  confirmRemoveCompleted: boolean;
  theme: "system" | "light" | "dark" | "high-contrast";
  /** Absent from an engine that predates the setting: comfortable. */
  density?: "comfortable" | "compact";
  onboardingCompleted: boolean;
  cacheQuotaBytes?: number | null;
  /** This engine's name; empty asks for the computer's name. */
  instanceName?: string | null;
  /** Keep the engine running in the background. */
  hubMode?: boolean | null;
  /** Free space kept on every drive; 0 is automatic. */
  diskReserveBytes?: number | null;
  /** Serve the queue to a browser on this computer. */
  webUi?: boolean | null;
}

export interface SettingsView {
  settings: Settings;
  repaired: boolean;
  maxActiveLimit: number;
  maxRetryAttempts: number;
  systemDownloadDir: string | null;
}

export interface AvailableTool {
  name: string;
  version: string;
  url: string;
  checksumSource: string;
  license: string;
  pinned: boolean;
}

export interface ToolsStatus {
  ready: boolean;
  ytDlpPath: string | null;
  ffmpegDir: string | null;
  ytDlpVersion: string | null;
  ffmpegVersion: string | null;
  jsRuntimeName?: string | null;
  jsRuntimePath?: string | null;
  jsRuntimeVersion?: string | null;
  installDir: string;
  available: AvailableTool[];
  problem: string | null;
}

interface SetupReport {
  version: string;
  cliOnPath: boolean;
  engineReady: boolean;
  defaultFolderReady: boolean;
  media: {
    ready: boolean;
    ytDlpVersion?: string | null;
    ffmpegVersion?: string | null;
    jsRuntimeName?: string | null;
    jsRuntimeVersion?: string | null;
    jsRuntimeReady: boolean;
  };
  torrent: { ready: boolean };
  browser: { hostInstalled: boolean; extensionPresent: boolean };
  agents: {
    detected: string[];
    connected: string[];
    ungrantedConnected: string[];
    totalGrantedAgents: number;
  };
}

interface BrowserSetupStatus {
  extensionDir: string | null;
  browsers: Array<{ browser: string; registered: boolean }>;
}

export interface SettingsPanelContext {
  dialogs: DialogManager;
  addOpenButton: HTMLButtonElement;
  refreshQueue(): Promise<void>;
  showFormNotice(message: string): void;
  onMediaToolsReady(wasReady: boolean, ready: boolean): void;
}

export interface SettingsPanel {
  readonly dialog: HTMLDialogElement;
  readonly view: SettingsView | null;
  readonly toolsStatus: ToolsStatus | null;
  readonly systemDownloadDir: string | null;
  readonly defaultDestinationDir: string | null;
  setDefaultDestinationDir(dir: string | null): void;
  show(category?: string): Promise<void>;
  load(): Promise<void>;
  refreshToolsStatus(): Promise<void>;
  showWebUiProblem(message: string): Promise<void>;
}

/** The reserve choices, in GiB as the engine counts them. */
const RESERVE_GIB = 1024 * 1024 * 1024;

/** Windows' own setting decides unless the user chose a specific appearance. */
function applyTheme(theme: Settings["theme"]): void {
  if (theme === "system") document.documentElement.removeAttribute("data-theme");
  else document.documentElement.setAttribute("data-theme", theme);
}

/** Comfortable is the token default, so only compact is written. */
function applyDensity(density: Settings["density"]): void {
  if (density === "compact") document.documentElement.setAttribute("data-density", "compact");
  else document.documentElement.removeAttribute("data-density");
}

/** Settings dialog, appearance, media tools, browser setup, CLI status, and sub-panels. */
export function createSettingsPanel(ctx: SettingsPanelContext): SettingsPanel {
  const { dialogs } = ctx;
  const openSettingsButton = required<HTMLButtonElement>("open-settings");
  const settingsDialog = required<HTMLDialogElement>("settings-dialog");
  const settingsCloseButton = required<HTMLButtonElement>("settings-close");
  const settingsCloseX = required<HTMLButtonElement>("settings-close-x");
  const settingsForm = required<HTMLFormElement>("settings-form");
  const settingsError = required<HTMLParagraphElement>("settings-error");
  const settingsStatus = required<HTMLParagraphElement>("settings-status");
  const settingsRepaired = required<HTMLParagraphElement>("settings-repaired");
  const concurrencyInput = required<HTMLInputElement>("setting-concurrency");
  const concurrencyValue = required<HTMLOutputElement>("setting-concurrency-value");
  const settingDestination = required<HTMLInputElement>("setting-destination");
  const settingChooseDestination = required<HTMLButtonElement>("setting-choose-destination");
  const settingClearDestination = required<HTMLButtonElement>("setting-clear-destination");
  const autoRetryInput = required<HTMLInputElement>("setting-auto-retry");
  const retryAttemptsInput = required<HTMLInputElement>("setting-retry-attempts");
  const retryAttemptsValue = required<HTMLOutputElement>("setting-retry-attempts-value");
  const closeToTrayInput = required<HTMLInputElement>("setting-close-to-tray");
  const instanceNameInput = required<HTMLInputElement>("setting-instance-name");
  const hubModeInput = required<HTMLInputElement>("setting-hub-mode");
  const webUiInput = required<HTMLInputElement>("setting-web-ui");
  const webUiOpen = required<HTMLButtonElement>("web-ui-open");
  const webUiSignOut = required<HTMLButtonElement>("web-ui-sign-out");
  const webUiState = required<HTMLParagraphElement>("web-ui-state");
  const diskReserveSelect = required<HTMLSelectElement>("setting-disk-reserve");
  const confirmRemoveInput = required<HTMLInputElement>("setting-confirm-remove");
  const powerModeInput = required<HTMLInputElement>("setting-power-mode");
  const themeSelect = required<HTMLSelectElement>("setting-theme");
  const densitySelect = required<HTMLSelectElement>("setting-density");
  const mediaToolsState = required<HTMLParagraphElement>("media-tools-state");
  const mediaToolsDetail = required<HTMLParagraphElement>("media-tools-detail");
  const mediaToolsLicence = required<HTMLParagraphElement>("media-tools-licence");
  const mediaToolsInstall = required<HTMLButtonElement>("media-tools-install");
  const mediaToolsLocate = required<HTMLButtonElement>("media-tools-locate");
  const trayNote = required<HTMLParagraphElement>("tray-note");
  const statsPanel = required<HTMLElement>("stats-panel");
  const onboarding = required<HTMLElement>("onboarding");
  const dismissOnboarding = required<HTMLButtonElement>("dismiss-onboarding");
  const onboardingMedia = required<HTMLElement>("onboarding-media");
  const onboardingOpenMedia = required<HTMLButtonElement>("onboarding-open-media");
  const onboardingAgents = required<HTMLElement>("onboarding-agents");
  const onboardingAgentsNote = required<HTMLElement>("onboarding-agents-note");
  const onboardingOpenAgents = required<HTMLButtonElement>("onboarding-open-agents");
  const setupChecklistGroup = required<HTMLElement>("setup-checklist-group");
  const setupChecklist = required<HTMLUListElement>("setup-checklist");
  const setupRecheck = required<HTMLButtonElement>("setup-recheck");
  const setupOpenAgents = required<HTMLButtonElement>("setup-open-agents");
  const mediaOpenSettings = required<HTMLButtonElement>("media-open-settings");
  const mediaSetupNeeded = required<HTMLElement>("media-setup-needed");
  const mediaInspectControls = required<HTMLElement>("media-inspect-controls");

  const installedComponents = required<HTMLParagraphElement>("installed-components");
  const browserSetupState = required<HTMLParagraphElement>("browser-setup-state");
  const browserSetupDetail = required<HTMLParagraphElement>("browser-setup-detail");
  const browserSetupSteps = required<HTMLOListElement>("browser-setup-steps");
  const browserManualDetails = required<HTMLDetailsElement>("browser-manual-details");
  const browserOpenFolder = required<HTMLButtonElement>("browser-open-folder");
  const browserCopyChrome = required<HTMLButtonElement>("browser-copy-chrome");
  const browserCopyEdge = required<HTMLButtonElement>("browser-copy-edge");
  const browserCopyFolder = required<HTMLButtonElement>("browser-copy-folder");
  const browserConnectChrome = required<HTMLButtonElement>("browser-connect-chrome");
  const browserConnectEdge = required<HTMLButtonElement>("browser-connect-edge");
  const browserExtensionPath = required<HTMLElement>("browser-extension-path");
  const cliState = required<HTMLParagraphElement>("cli-state");
  const cliDetail = required<HTMLParagraphElement>("cli-detail");
  const cliPath = required<HTMLElement>("cli-path");

  let settingsView: SettingsView | null = null;
  let toolsStatus: ToolsStatus | null = null;
  let systemDownloadDir: string | null = null;
  let defaultDestinationDir: string | null = null;
  let settingsQueue: Promise<void> = Promise.resolve();

  function showSettingsError(error: unknown): void {
    const message = errorMessage(error);
    settingsError.textContent = message;
    settingsError.hidden = false;
    settingsStatus.hidden = true;
    dialogs.announceProblem(message);
  }

  function clearSettingsError(): void {
    settingsError.textContent = "";
    settingsError.hidden = true;
    settingsStatus.hidden = true;
  }

  function setSettingsStatus(message: string): void {
    settingsStatus.textContent = message;
    settingsStatus.hidden = false;
    dialogs.announce(message);
  }

  const settingsFeedback = {
    showError: showSettingsError,
    clearError: clearSettingsError,
    setStatus: setSettingsStatus,
  };

  const cachePanel = createCachePanel({
    ...settingsFeedback,
    updateQuota: (cacheQuotaBytes, note) => changeSetting({ cacheQuotaBytes }, note),
  });
  const lanPanel = createLanPanel({
    ...settingsFeedback,
    announce: dialogs.announce,
    errorMessage,
  });
  const sharePanel = createSharePanel({
    ...settingsFeedback,
    settingsDialog,
    announce: dialogs.announce,
  });
  const rulesPanel = createRulesPanel(settingsFeedback);
  const agentsPanel = createAgentsPanel(settingsFeedback);

  const settingsCategories = [...settingsDialog.querySelectorAll<HTMLButtonElement>("[data-settings-category]")];
  for (const button of settingsCategories) {
    button.addEventListener("click", () => {
      const category = button.dataset.settingsCategory;
      for (const choice of settingsCategories) choice.setAttribute("aria-pressed", String(choice === button));
      for (const panel of settingsForm.querySelectorAll<HTMLElement>("[data-settings-panel]")) {
        panel.hidden = panel.dataset.settingsPanel !== category || panel.dataset.unavailable === "true";
      }
      settingsForm.setAttribute("aria-label", `${button.textContent} settings`);
      settingsForm.scrollTop = 0;
    });
  }

  // Every setting saves the moment it changes, so the settings form has nothing
  // to submit. Without this, Enter in the folder field triggers implicit
  // submission, which navigates the webview away from the application.
  settingsForm.addEventListener("submit", (event) => event.preventDefault());

  openSettingsButton.addEventListener("click", () => void show());
  settingsCloseButton.addEventListener("click", () => settingsDialog.close());
  settingsCloseX.addEventListener("click", () => settingsDialog.close());
  settingsDialog.addEventListener("close", () => {
    sharePanel.onDialogClose();
    lanPanel.onDialogClose();
    dialogs.restoreDialogFocus(openSettingsButton, settingsDialog);
  });

  required<HTMLAnchorElement>("project-attribution").addEventListener("click", async (event) => {
    event.preventDefault();
    clearSettingsError();
    try {
      await invoke("open_project_page");
    } catch (error) {
      showSettingsError(error);
    }
  });

  async function refreshCliStatus(): Promise<void> {
    const path = await invoke<string | null>("cli_path").catch(() => null);
    cliPath.hidden = !path;
    cliPath.textContent = path ?? "";
    if (path) {
      cliState.textContent = "The fetchpath command is installed.";
      cliDetail.textContent =
        "Open a new terminal and type fetchpath --help. Downloads started there use the same engine and appear in this list.";
    } else {
      cliState.textContent = "The fetchpath command is not installed beside this copy of Fetchpath.";
      cliDetail.textContent = "The Fetchpath installer adds it. A copy run from elsewhere, such as a development build, does not.";
    }
  }

  async function refreshInstalledComponents(): Promise<void> {
    try {
      const summary = await invoke<string | null>("installed_components");
      installedComponents.textContent =
        summary ?? "This copy was not installed with the Fetchpath installer, so it has no component list.";
    } catch {
      installedComponents.textContent = "Could not read the installed components.";
    }
  }

  function renderBrowserSetup(status: BrowserSetupStatus): void {
    // Firefox's host is registered for a future signed add-on, but it cannot load
    // this extension yet, so it is not named as connected.
    const registered = status.browsers
      .filter((entry) => entry.registered && entry.browser !== "Firefox")
      .map((entry) => entry.browser);
    const ready = registered.length > 0 && status.extensionDir !== null;
    browserSetupState.dataset.ready = String(ready);
    browserSetupState.textContent = ready
      ? `Fetchpath is connected to ${registered.join(" and ")}. Add the extension to your browser to send links.`
      : "The browser connection is set up by the Fetchpath installer.";
    browserSetupDetail.textContent = ready
      ? "Firefox is not supported yet: it only accepts signed add-ons."
      : "This copy was not installed with the installer, so browser capture is unavailable here.";
    browserSetupSteps.hidden = !ready;
    browserManualDetails.hidden = !ready;
    browserExtensionPath.textContent = status.extensionDir ?? "";
    for (const button of [
      browserConnectChrome,
      browserConnectEdge,
      browserOpenFolder,
      browserCopyFolder,
      browserCopyChrome,
      browserCopyEdge,
    ]) {
      button.hidden = !ready;
    }
  }

  async function refreshBrowserSetup(): Promise<void> {
    try {
      renderBrowserSetup(await invoke<BrowserSetupStatus>("browser_setup_status"));
    } catch (error) {
      browserSetupState.textContent = "Could not check the browser extension.";
      browserSetupDetail.textContent = errorMessage(error);
    }
  }

  browserOpenFolder.addEventListener("click", async () => {
    clearSettingsError();
    try {
      await invoke("reveal_extension_folder");
      dialogs.announce("Opened the extension folder in File Explorer.");
    } catch (error) {
      showSettingsError(error);
    }
  });

  browserCopyFolder.addEventListener("click", async () => {
    const folder = browserExtensionPath.textContent ?? "";
    if (!folder) return;
    clearSettingsError();
    try {
      await navigator.clipboard.writeText(folder);
      setSettingsStatus("Copied the extension folder. Paste it into the folder picker's address bar.");
    } catch (error) {
      showSettingsError(error);
    }
  });

  for (const [button, browser] of [
    [browserConnectChrome, "Chrome"],
    [browserConnectEdge, "Edge"],
  ] as const) {
    button.addEventListener("click", async () => {
      clearSettingsError();
      try {
        // Copied first: the folder picker the browser shows next needs it.
        await navigator.clipboard.writeText(browserExtensionPath.textContent ?? "");
        await invoke("connect_browser", { browser });
        setSettingsStatus(
          `Opened ${browser}'s extensions page and the folder, and copied the folder path. Turn on Developer mode, choose Load unpacked and paste it.`,
        );
      } catch (error) {
        showSettingsError(error);
      }
    });
  }

  for (const [button, address, browser] of [
    [browserCopyChrome, "chrome://extensions", "Chrome"],
    [browserCopyEdge, "edge://extensions", "Edge"],
  ] as const) {
    button.addEventListener("click", async () => {
      clearSettingsError();
      try {
        // Browsers refuse to open their internal pages from another program, so
        // the address is copied for the person to paste instead.
        await navigator.clipboard.writeText(address);
        setSettingsStatus(`Copied ${address}. Paste it into ${browser}'s address bar.`);
      } catch (error) {
        showSettingsError(error);
      }
    });
  }

  function applySettingsToForm(view: SettingsView): void {
    const { settings } = view;
    systemDownloadDir = view.systemDownloadDir;
    defaultDestinationDir = settings.defaultDestinationDir ?? view.systemDownloadDir;

    settingsRepaired.hidden = !view.repaired;
    concurrencyInput.max = String(view.maxActiveLimit);
    concurrencyInput.value = String(settings.maxActiveDownloads);
    concurrencyValue.textContent = String(settings.maxActiveDownloads);
    retryAttemptsInput.max = String(view.maxRetryAttempts);
    retryAttemptsInput.value = String(settings.autoRetryMaxAttempts);
    retryAttemptsValue.textContent = String(settings.autoRetryMaxAttempts);
    retryAttemptsInput.disabled = !settings.autoRetry;

    settingDestination.value = settings.defaultDestinationDir ?? "";
    settingDestination.placeholder = view.systemDownloadDir ?? "Your Windows Downloads folder";
    settingClearDestination.hidden = !settings.defaultDestinationDir;

    autoRetryInput.checked = settings.autoRetry;
    closeToTrayInput.checked = settings.closeToTray;
    instanceNameInput.value = settings.instanceName ?? "";
    hubModeInput.checked = settings.hubMode === true;
    webUiInput.checked = settings.webUi === true;
    webUiOpen.disabled = !webUiInput.checked;
    webUiSignOut.disabled = !webUiInput.checked;
    // One byte is the engine's "no limit"; it would round to Automatic below.
    for (const stale of diskReserveSelect.querySelectorAll("option[data-custom]")) stale.remove();
    const reserveBytes = settings.diskReserveBytes ?? 0;
    const whole = reserveBytes % RESERVE_GIB === 0;
    const reserveChoice = reserveBytes === 1 ? "off" : whole ? String(reserveBytes / RESERVE_GIB) : `b${reserveBytes}`;
    if (![...diskReserveSelect.options].some((option) => option.value === reserveChoice)) {
      // A value set elsewhere, such as from the command line, is shown as it is.
      const custom = new Option(whole ? `${reserveChoice} GiB` : `${formatBytes(reserveBytes)} (set elsewhere)`, reserveChoice);
      custom.dataset.custom = "";
      diskReserveSelect.add(custom);
    }
    diskReserveSelect.value = reserveChoice;
    confirmRemoveInput.checked = settings.confirmRemoveCompleted;
    powerModeInput.checked = settings.powerMode;
    themeSelect.value = settings.theme;
    densitySelect.value = settings.density ?? "comfortable";

    applyTheme(settings.theme);
    applyDensity(settings.density);
    statsPanel.hidden = !settings.powerMode;
    onboarding.hidden = settings.onboardingCompleted;
    trayNote.textContent = settings.closeToTray
      ? "Closing this window hides it in the notification area. Downloads keep running."
      : "Closing this window exits the desktop and its tray icon. Downloads keep running in Fetchpath's engine.";
  }

  async function load(): Promise<void> {
    try {
      settingsView = await invoke<SettingsView>("get_settings");
      applySettingsToForm(settingsView);
      void refreshSetupCheck();
    } catch (error) {
      showSettingsError(error);
    }
  }

  /** Sends one changed field, serialized so rapid changes never overwrite each other. */
  function changeSetting(patch: Partial<Settings>, note?: string): Promise<void> {
    const run = async (): Promise<void> => {
      if (!settingsView) return;
      clearSettingsError();
      const next = { ...settingsView.settings, ...patch };
      try {
        settingsView = await invoke<SettingsView>("update_settings", { next });
        applySettingsToForm(settingsView);
        if (note) setSettingsStatus(note);
        await ctx.refreshQueue();
      } catch (error) {
        showSettingsError(error);
        // The backend refused, so the control must go back to the stored value
        // rather than showing a change that did not happen.
        applySettingsToForm(settingsView);
      }
    };
    settingsQueue = settingsQueue.then(run, run);
    return settingsQueue;
  }

  /** Settings changed from outside the dialog, where there is no form to update. */
  async function changeSettingWithoutDialog(patch: Partial<Settings>): Promise<boolean> {
    if (!settingsView) return false;
    try {
      settingsView = await invoke<SettingsView>("update_settings", { next: { ...settingsView.settings, ...patch } });
      applySettingsToForm(settingsView);
      return true;
    } catch {
      return false;
    }
  }

  concurrencyInput.addEventListener("input", () => {
    concurrencyValue.textContent = concurrencyInput.value;
  });
  concurrencyInput.addEventListener("change", () => {
    void changeSetting({ maxActiveDownloads: Number(concurrencyInput.value) });
  });

  retryAttemptsInput.addEventListener("input", () => {
    retryAttemptsValue.textContent = retryAttemptsInput.value;
  });
  retryAttemptsInput.addEventListener("change", () => {
    void changeSetting({ autoRetryMaxAttempts: Number(retryAttemptsInput.value) });
  });

  autoRetryInput.addEventListener("change", () => void changeSetting({ autoRetry: autoRetryInput.checked }));
  closeToTrayInput.addEventListener("change", () => void changeSetting({ closeToTray: closeToTrayInput.checked }));
  instanceNameInput.addEventListener("change", () =>
    void changeSetting({ instanceName: instanceNameInput.value.trim() }, "Name saved."),
  );
  diskReserveSelect.addEventListener("change", () =>
    diskReserveSelect.value.startsWith("b")
      ? undefined
      : void changeSetting(
          { diskReserveBytes: diskReserveSelect.value === "off" ? 1 : Number(diskReserveSelect.value) * RESERVE_GIB },
          "Saved.",
        ),
  );
  hubModeInput.addEventListener("change", () =>
    void changeSetting(
      { hubMode: hubModeInput.checked },
      hubModeInput.checked
        ? "Always on: Fetchpath keeps running in the background and starts when you sign in."
        : "Always on is off: Fetchpath stops a minute after it has nothing left to do.",
    ),
  );
  webUiInput.addEventListener("change", () =>
    void changeSetting(
      { webUi: webUiInput.checked },
      webUiInput.checked ? "Web UI on. Use Open in browser to sign a browser in." : "Web UI off. Browsers are signed out.",
    ),
  );
  webUiOpen.addEventListener("click", async () => {
    clearSettingsError();
    try {
      await invoke("open_web_ui");
      webUiState.textContent = "Opened in your browser.";
    } catch (error) {
      showSettingsError(error);
    }
  });
  webUiSignOut.addEventListener("click", async () => {
    clearSettingsError();
    try {
      await invoke("sign_out_browsers");
      webUiState.textContent = "Every browser is signed out.";
      dialogs.announce("Every browser is signed out.");
    } catch (error) {
      showSettingsError(error);
    }
  });
  confirmRemoveInput.addEventListener("change", () =>
    void changeSetting({ confirmRemoveCompleted: confirmRemoveInput.checked }),
  );
  powerModeInput.addEventListener("change", () => void changeSetting({ powerMode: powerModeInput.checked }));
  themeSelect.addEventListener("change", () => void changeSetting({ theme: themeSelect.value as Settings["theme"] }));
  densitySelect.addEventListener("change", () => void changeSetting({ density: densitySelect.value as Settings["density"] }));

  settingChooseDestination.addEventListener("click", async () => {
    clearSettingsError();
    try {
      const selected = await open({
        directory: true,
        title: "Choose the default save folder",
        defaultPath: settingDestination.value || systemDownloadDir || undefined,
      });
      if (typeof selected === "string") {
        await changeSetting({ defaultDestinationDir: selected }, "Default save folder updated.");
      }
    } catch (error) {
      showSettingsError(error);
    }
  });

  settingClearDestination.addEventListener("click", () => {
    void changeSetting({ defaultDestinationDir: null }, "New downloads will use your Windows Downloads folder.");
  });

  dismissOnboarding.addEventListener("click", async () => {
    onboarding.hidden = true;
    if (!(await changeSettingWithoutDialog({ onboardingCompleted: true }))) {
      onboarding.hidden = false;
      ctx.showFormNotice("Could not save that you dismissed this. It will show again next time.");
      return;
    }
    ctx.addOpenButton.focus({ preventScroll: true });
  });

  onboardingOpenMedia.addEventListener("click", () => void show("integrations"));
  onboardingOpenAgents.addEventListener("click", () => void show("automation"));
  setupOpenAgents.addEventListener("click", () => void show("automation"));
  setupRecheck.addEventListener("click", async () => {
    clearSettingsError();
    setupRecheck.disabled = true;
    try {
      await Promise.all([refreshToolsStatus(), refreshBrowserSetup(), refreshSetupCheck()]);
      setSettingsStatus("Requirements check updated.");
    } finally {
      setupRecheck.disabled = false;
    }
  });
  mediaOpenSettings.addEventListener("click", () => void show("integrations"));

  function renderSetupChecklist(report: SetupReport): void {
    const rows: Array<[string, string, string]> = [
      [
        report.media.ready ? "ok" : "missing",
        "Video & audio",
        report.media.ready
          ? `yt-dlp ${report.media.ytDlpVersion ?? ""} · ffmpeg ${report.media.ffmpegVersion ?? ""}`.trim()
          : "yt-dlp and ffmpeg not installed yet — use Download and set up below.",
      ],
      [
        report.media.jsRuntimeReady ? "ok" : "optional",
        "JS runtime (YouTube)",
        report.media.jsRuntimeReady
          ? `${report.media.jsRuntimeName ?? "runtime"} ${report.media.jsRuntimeVersion ?? ""}`.trim()
          : "Not found (optional; install Deno 2.3+ or Node.js 22+ for full YouTube formats).",
      ],
      [
        report.torrent.ready ? "ok" : "optional",
        "Torrent helper",
        report.torrent.ready ? "Installed" : "Optional — enable the Torrent component in Fetchpath Setup.",
      ],
      [
        report.browser.hostInstalled && report.browser.extensionPresent ? "ok" : "optional",
        "Browser extension",
        report.browser.hostInstalled && report.browser.extensionPresent
          ? "Native host and extension files ready"
          : "Optional — enable Browser integration in Fetchpath Setup.",
      ],
      [
        report.agents.connected.length > 0 && report.agents.ungrantedConnected.length === 0
          ? "ok"
          : report.agents.detected.length > 0
            ? "ready"
            : "optional",
        "AI agents (MCP)",
        report.agents.connected.length > 0
          ? report.agents.ungrantedConnected.length > 0
            ? `Connected (${report.agents.connected.join(", ")}) — grant a folder in Agents and rules`
            : `Connected and granted (${report.agents.connected.join(", ")})`
          : report.agents.detected.length > 0
            ? `Detected (${report.agents.detected.join(", ")}) — ready to connect`
            : "Use Agents and rules to copy MCP config for any host.",
      ],
    ];
    setupChecklist.replaceChildren(
      ...rows.map(([state, label, detail]) => {
        const li = document.createElement("li");
        li.dataset.state = state;
        const name = document.createElement("strong");
        name.textContent = label;
        const text = document.createElement("span");
        text.textContent = detail;
        li.append(name, text);
        return li;
      }),
    );

    const unconnected = report.agents.detected.filter((id) => !report.agents.connected.includes(id));
    if (unconnected.length > 0) {
      onboardingAgentsNote.textContent = `Detected ${unconnected.join(", ")} on this PC. Connect in one click in Settings.`;
      onboardingAgents.hidden = false;
    } else {
      onboardingAgents.hidden = true;
    }
  }

  async function refreshSetupCheck(): Promise<void> {
    try {
      const report = JSON.parse(await invoke<string>("setup_check")) as SetupReport;
      delete setupChecklistGroup.dataset.unavailable;
      const active = settingsCategories.find((b) => b.getAttribute("aria-pressed") === "true")?.dataset.settingsCategory;
      setupChecklistGroup.hidden = active !== "integrations";
      renderSetupChecklist(report);
    } catch {
      setupChecklistGroup.dataset.unavailable = "true";
      setupChecklistGroup.hidden = true;
      onboardingAgents.hidden = true;
    }
  }

  function updateMediaAvailability(): void {
    const ready = toolsStatus?.ready ?? false;
    mediaSetupNeeded.hidden = ready;
    mediaInspectControls.hidden = !ready;
    // Tools found on this PC (or already set up) need no first-run prompt.
    onboardingMedia.hidden = ready;
  }

  function renderToolsStatus(status: ToolsStatus): void {
    mediaToolsState.dataset.ready = String(status.ready);
    if (status.ready) {
      mediaToolsState.textContent = "Ready to save video and audio";
      const jsPart = status.jsRuntimeName
        ? `JS runtime: ${status.jsRuntimeName}${status.jsRuntimeVersion ? ` ${status.jsRuntimeVersion}` : ""}`
        : "No JS runtime found (optional: install Deno or Node.js 22+ for full YouTube formats)";
      mediaToolsDetail.textContent =
        [status.ytDlpVersion, status.ffmpegVersion, jsPart].filter(Boolean).join(" · ") || status.installDir;
    } else if (status.problem) {
      mediaToolsState.textContent = "Found, but they would not run";
      mediaToolsDetail.textContent = status.problem;
    } else {
      mediaToolsState.textContent = "Not set up yet";
      mediaToolsDetail.textContent =
        "Fetchpath does not include yt-dlp or ffmpeg. Download them here, or point Fetchpath at a folder that already has them.";
    }

    // Only offer the download when this build actually carries a checksum to
    // check the result against. Otherwise the honest offer is the manual one.
    const unpinned = status.available.filter((tool) => !tool.pinned);
    mediaToolsInstall.disabled = status.ready || unpinned.length > 0;
    mediaToolsInstall.textContent = status.ready ? "Already set up" : "Download and set up";

    if (unpinned.length && !status.ready) {
      mediaToolsLicence.textContent =
        `This build has no recorded checksum for ${unpinned.map((tool) => tool.name).join(" and ")}, ` +
        "so Fetchpath will not download it. Install it yourself and choose the folder below.";
    } else {
      mediaToolsLicence.textContent = status.available
        .map((tool) => `${tool.name} ${tool.version} — ${tool.license}`)
        .join(" · ");
    }
  }

  async function refreshToolsStatus(): Promise<void> {
    const wasReady = toolsStatus?.ready === true;
    try {
      toolsStatus = await invoke<ToolsStatus>("media_tools_status");
      renderToolsStatus(toolsStatus);
    } catch (error) {
      showSettingsError(error);
    }
    updateMediaAvailability();
    ctx.onMediaToolsReady(wasReady, toolsStatus?.ready === true);
  }

  mediaToolsInstall.addEventListener("click", async () => {
    clearSettingsError();
    mediaToolsInstall.disabled = true;
    mediaToolsInstall.textContent = "Downloading…";
    dialogs.announce("Downloading the media tools. This can take a minute.");
    try {
      toolsStatus = await invoke<ToolsStatus>("install_media_tools");
      renderToolsStatus(toolsStatus);
      dialogs.announce(toolsStatus.ready ? "Media tools are ready." : "The media tools were not set up.");
    } catch (error) {
      showSettingsError(error);
      if (toolsStatus) {
        renderToolsStatus(toolsStatus);
      } else {
        mediaToolsInstall.disabled = false;
        mediaToolsInstall.textContent = "Download and set up";
      }
    } finally {
      updateMediaAvailability();
      if (mediaToolsInstall.isConnected && !mediaToolsInstall.disabled) {
        mediaToolsInstall.focus({ preventScroll: true });
      }
    }
  });

  mediaToolsLocate.addEventListener("click", async () => {
    clearSettingsError();
    try {
      const selected = await open({
        directory: true,
        title: "Choose the folder that holds yt-dlp and ffmpeg",
        defaultPath: toolsStatus?.installDir,
      });
      if (typeof selected !== "string") return;
      toolsStatus = await invoke<ToolsStatus>("use_media_tools_dir", { directory: selected });
      renderToolsStatus(toolsStatus);
      updateMediaAvailability();
      dialogs.announce(toolsStatus.ready ? "Media tools are ready." : "Those programs could not be used.");
    } catch (error) {
      showSettingsError(error);
    }
  });

  async function show(category?: string): Promise<void> {
    if (category) settingsCategories.find((button) => button.dataset.settingsCategory === category)?.click();
    clearSettingsError();
    dialogs.openDialog(
      settingsDialog,
      settingsCategories.find((button) => button.getAttribute("aria-pressed") === "true") ?? settingsCloseButton,
    );
    await Promise.all([
      load(),
      refreshToolsStatus(),
      refreshBrowserSetup(),
      refreshSetupCheck(),
      refreshInstalledComponents(),
      refreshCliStatus(),
      agentsPanel.refresh(),
      agentsPanel.refreshHosts(),
      rulesPanel.refresh(),
      cachePanel.refresh(),
      lanPanel.refresh(),
      sharePanel.refresh(),
    ]);
  }

  async function showWebUiProblem(message: string): Promise<void> {
    await show("general");
    webUiState.textContent = message;
    dialogs.announceProblem(message);
  }

  return {
    dialog: settingsDialog,
    get view() {
      return settingsView;
    },
    get toolsStatus() {
      return toolsStatus;
    },
    get systemDownloadDir() {
      return systemDownloadDir;
    },
    get defaultDestinationDir() {
      return defaultDestinationDir;
    },
    setDefaultDestinationDir(dir: string | null) {
      defaultDestinationDir = dir;
    },
    show,
    load,
    refreshToolsStatus,
    showWebUiProblem,
  };
}
