import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { required } from "./dom";
import { formatBytes } from "./format";

export type AgentPermissionMode = "ask" | "scoped" | "auto" | "full_access";

/** One agent's access, as the engine keeps it. */
export interface AgentView {
  name: string;
  folders: string[];
  maxBytes: number;
  maxNewJobsPerHour: number;
  /** Inside its folders (or everywhere in full-access), size/rate/peer holds are lifted. */
  automatic: boolean;
  permissionMode: AgentPermissionMode;
}

interface AgentHostState {
  id?: string;
  label?: string;
  available?: boolean;
  registered?: boolean;
  owned?: boolean;
  connection?: string;
}

interface UniversalSnippets {
  generic_command?: string;
  mcp_json?: string;
  vscode_json?: string;
  codex_toml?: string;
  claude_cli?: string;
  codex_cli?: string;
  skill_install?: string;
}

interface AgentSetupStatus {
  codex?: AgentHostState;
  claude_code?: AgentHostState;
  hosts?: AgentHostState[];
  universal?: UniversalSnippets;
}

export interface AgentsPanelContext {
  showError(error: unknown): void;
  clearError(): void;
  setStatus(message: string): void;
}

export interface AgentsPanel {
  refresh(): Promise<void>;
  refreshHosts(): Promise<void>;
}

const DEFAULT_AGENT_BYTES = 1024 * 1024 * 1024;
const DEFAULT_AGENT_PER_HOUR = 20;
const MIB = 1024 * 1024;

const MODE_OPTIONS: Array<{ value: AgentPermissionMode; label: string; short: string }> = [
  { value: "ask", label: "Ask every time", short: "Ask" },
  { value: "scoped", label: "Folders & limits", short: "Scoped" },
  { value: "auto", label: "Auto in folders", short: "Auto" },
  { value: "full_access", label: "Full access (skip approvals)", short: "Full access" },
];

function effectiveMode(agent: AgentView | undefined): AgentPermissionMode {
  if (!agent) return "scoped";
  if (agent.permissionMode) return agent.permissionMode;
  return agent.automatic ? "auto" : "scoped";
}

function modeShortLabel(mode: AgentPermissionMode): string {
  return MODE_OPTIONS.find((item) => item.value === mode)?.short ?? "Scoped";
}

function modeSelect(
  current: AgentPermissionMode,
  ariaLabel: string,
  compact = false,
): HTMLSelectElement {
  const select = document.createElement("select");
  select.className = compact ? "agent-mode-select compact" : "agent-mode-select";
  select.setAttribute("aria-label", ariaLabel);
  for (const option of MODE_OPTIONS) {
    const el = document.createElement("option");
    el.value = option.value;
    el.textContent = option.label;
    el.selected = option.value === current;
    select.append(el);
  }
  return select;
}

function modeStatusMessage(name: string, mode: AgentPermissionMode): string {
  switch (mode) {
    case "ask":
      return `${name} set to Ask every time: every download waits for your approval.`;
    case "scoped":
      return `${name} set to Folders & limits: allowed inside granted folders within its size and hourly limits.`;
    case "auto":
      return `${name} set to Auto in folders: size, hourly, and torrent peer holds are lifted inside its folders.`;
    case "full_access":
      return `${name} set to Full access: all download approvals are bypassed anywhere on disk.`;
  }
}

function agentButton(label: string, name: string, action: string, accessible: string, danger = false): HTMLButtonElement {
  const button = document.createElement("button");
  button.type = "button";
  button.className = danger ? "secondary danger" : "secondary";
  button.textContent = label;
  button.dataset.agent = name;
  button.dataset.agentAction = action;
  button.setAttribute("aria-label", accessible);
  return button;
}

async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    return false;
  }
}

/** AI agent access policies (FP-066), harness-style permission modes, and MCP host linking. */
export function createAgentsPanel(ctx: AgentsPanelContext): AgentsPanel {
  const agentsList = required<HTMLDivElement>("agents-list");
  const agentNewName = required<HTMLInputElement>("agent-new-name");
  const agentAdd = required<HTMLButtonElement>("agent-add");
  const agentHosts = required<HTMLElement>("agent-hosts");

  let agents: AgentView[] = [];
  let lastStatus: AgentSetupStatus | null = null;

  async function saveAgent(agent: AgentView, message: string, focus?: string): Promise<void> {
    ctx.clearError();
    try {
      agents = await invoke<AgentView[]>("set_agent", {
        name: agent.name,
        folders: agent.folders,
        maxBytes: agent.maxBytes,
        maxNewJobsPerHour: agent.maxNewJobsPerHour,
        automatic: agent.permissionMode === "auto" || agent.permissionMode === "full_access",
        permissionMode: agent.permissionMode,
      });
      render();
      if (lastStatus) renderHosts(lastStatus);
      ctx.setStatus(message);
    } catch (error) {
      ctx.showError(error);
    }
    if (focus) {
      agentsList.querySelector<HTMLElement>(focus)?.focus();
    }
  }

  async function setAgentMode(name: string, mode: AgentPermissionMode, focus?: string): Promise<void> {
    const existing = agents.find((candidate) => candidate.name === name);
    const next: AgentView = existing
      ? {
          ...existing,
          permissionMode: mode,
          automatic: mode === "auto" || mode === "full_access",
        }
      : {
          name,
          folders: [],
          maxBytes: DEFAULT_AGENT_BYTES,
          maxNewJobsPerHour: DEFAULT_AGENT_PER_HOUR,
          automatic: mode === "auto" || mode === "full_access",
          permissionMode: mode,
        };
    await saveAgent(next, modeStatusMessage(name, mode), focus);
  }

  async function setModeForAll(names: string[], mode: AgentPermissionMode): Promise<void> {
    ctx.clearError();
    try {
      for (const name of names) {
        const existing = agents.find((candidate) => candidate.name === name);
        agents = await invoke<AgentView[]>("set_agent", {
          name,
          folders: existing?.folders ?? [],
          maxBytes: existing?.maxBytes ?? DEFAULT_AGENT_BYTES,
          maxNewJobsPerHour: existing?.maxNewJobsPerHour ?? DEFAULT_AGENT_PER_HOUR,
          automatic: mode === "auto" || mode === "full_access",
          permissionMode: mode,
        });
      }
      render();
      if (lastStatus) renderHosts(lastStatus);
      ctx.setStatus(`Updated ${names.length} agents to ${modeShortLabel(mode)}.`);
    } catch (error) {
      ctx.showError(error);
    }
  }

  async function pickAndGrantFolder(name: string, focus?: string): Promise<void> {
    ctx.clearError();
    try {
      const selected = await open({ directory: true, title: `Choose a folder ${name} may save into` });
      if (typeof selected !== "string") return;
      const existing = agents.find((candidate) => candidate.name === name);
      if (existing) {
        if (existing.folders.some((folder) => folder.toLowerCase() === selected.toLowerCase())) {
          ctx.setStatus(`${name} can already save into ${selected}.`);
          return;
        }
        await saveAgent(
          { ...existing, folders: [...existing.folders, selected] },
          `${name} can now save into ${selected}.`,
          focus,
        );
      } else {
        await saveAgent(
          {
            name,
            folders: [selected],
            maxBytes: DEFAULT_AGENT_BYTES,
            maxNewJobsPerHour: DEFAULT_AGENT_PER_HOUR,
            automatic: false,
            permissionMode: "scoped",
          },
          `${name} can now save into ${selected}.`,
          focus,
        );
      }
    } catch (error) {
      ctx.showError(error);
    }
  }

  function render(): void {
    agentsList.replaceChildren();
    if (!agents.length) {
      const empty = document.createElement("p");
      empty.className = "field-note";
      empty.textContent =
        "No per-agent policies configured yet. Every request waits for you until you choose a permission mode or grant a folder.";
      agentsList.append(empty);
      return;
    }
    for (const agent of agents) {
      const mode = effectiveMode(agent);
      const card = document.createElement("section");
      card.className = "agent-card";
      card.setAttribute("aria-labelledby", `agent-${agent.name}`);

      const head = document.createElement("div");
      head.className = "agent-card-head";
      const heading = document.createElement("h3");
      heading.id = `agent-${agent.name}`;
      heading.textContent = agent.name;
      const badge = document.createElement("span");
      badge.className = "agent-mode-badge";
      badge.dataset.mode = mode;
      badge.textContent = modeShortLabel(mode);
      head.append(heading, badge);

      const summary = document.createElement("p");
      summary.className = "hint field-note";
      const where = agent.folders.length === 1 ? "this folder" : "these folders";
      if (mode === "full_access") {
        summary.textContent =
          "Full access: saves anywhere on disk without waiting for approvals, size caps, hourly limits, or torrent prompts.";
      } else if (mode === "ask") {
        summary.textContent =
          "Ask every time: every download waits for your approval (any saved folders and limits stay preserved if you switch back).";
      } else if (!agent.folders.length) {
        summary.textContent = "No folders yet: everything it asks for waits for you.";
      } else if (mode === "auto") {
        summary.textContent = `Auto in folders: saves into ${where} without asking, whatever the size or how many. Anything elsewhere still waits for you.`;
      } else {
        summary.textContent = `Saves into ${where} without asking, up to ${formatBytes(agent.maxBytes)} a download and ${agent.maxNewJobsPerHour} downloads an hour.`;
      }
      card.append(head, summary);

      const modeRow = document.createElement("div");
      modeRow.className = "agent-mode-row";
      const modeLabel = document.createElement("label");
      modeLabel.textContent = "Permission mode";
      modeLabel.htmlFor = `agent-mode-${agent.name}`;
      const select = modeSelect(mode, `Permission mode for ${agent.name}`);
      select.id = `agent-mode-${agent.name}`;
      select.dataset.agent = agent.name;
      select.dataset.agentMode = "true";
      modeRow.append(modeLabel, select);
      card.append(modeRow);

      if (mode === "full_access") {
        const warning = document.createElement("p");
        warning.className = "agent-full-warning";
        warning.textContent =
          "Approvals are bypassed for this agent. Fetchpath still never overwrites existing files and blocks access to settings, policies, shares, and credentials.";
        card.append(warning);
      }

      if (agent.folders.length) {
        const list = document.createElement("ul");
        list.className = "agent-folders";
        list.setAttribute("aria-label", `Folders for ${agent.name}`);
        agent.folders.forEach((folder, index) => {
          const item = document.createElement("li");
          const path = document.createElement("code");
          path.textContent = folder;
          const remove = agentButton("Remove", agent.name, "remove-folder", `Remove ${folder} from ${agent.name}`);
          remove.dataset.index = String(index);
          item.append(path, remove);
          list.append(item);
        });
        card.append(list);
      }

      if (mode === "scoped") {
        const limits = document.createElement("div");
        limits.className = "agent-limits";
        const sizeLabel = document.createElement("label");
        sizeLabel.textContent = "Largest download (MB)";
        const size = document.createElement("input");
        size.type = "number";
        size.min = "1";
        size.value = String(Math.max(1, Math.round(agent.maxBytes / MIB)));
        size.id = `agent-size-${agent.name}`;
        sizeLabel.htmlFor = size.id;
        const sizeWrap = document.createElement("div");
        sizeWrap.append(sizeLabel, size);
        const rateLabel = document.createElement("label");
        rateLabel.textContent = "Downloads an hour";
        const rate = document.createElement("input");
        rate.type = "number";
        rate.min = "1";
        rate.max = "1000";
        rate.value = String(agent.maxNewJobsPerHour);
        rate.id = `agent-rate-${agent.name}`;
        rateLabel.htmlFor = rate.id;
        const rateWrap = document.createElement("div");
        rateWrap.append(rateLabel, rate);
        limits.append(sizeWrap, rateWrap, agentButton("Save limits", agent.name, "save-limits", `Save limits for ${agent.name}`));
        card.append(limits);
      }

      const actions = document.createElement("div");
      actions.className = "agent-actions";
      if (mode !== "full_access") {
        actions.append(
          agentButton("Add folder…", agent.name, "add-folder", `Add a folder for ${agent.name}`),
        );
      }
      actions.append(
        agentButton("Revoke access", agent.name, "revoke", `Revoke access for ${agent.name}`, true),
      );
      card.append(actions);
      agentsList.append(card);
    }
  }

  function normalizedHosts(status: AgentSetupStatus): Array<AgentHostState & { id: string; label: string }> {
    if (Array.isArray(status.hosts) && status.hosts.length > 0) {
      return status.hosts.map((host) => ({
        ...host,
        id: host.id ?? "agent",
        label: host.label ?? host.id ?? "Agent",
      }));
    }
    return [
      { ...(status.codex ?? {}), id: "codex", label: "Codex" },
      { ...(status.claude_code ?? {}), id: "claude-code", label: "Claude Code" },
    ];
  }

  function renderHosts(status: AgentSetupStatus): void {
    const hosts = normalizedHosts(status);
    const detected = hosts.filter((h) => h.available === true);
    const missing = hosts.filter((h) => h.available !== true);
    const connectable = detected.filter((h) => !h.registered);
    const connectedIds = detected.filter((h) => h.registered === true).map((h) => h.id);
    const controllableNames = Array.from(new Set([...connectedIds, ...agents.map((a) => a.name)]));
    const nodes: HTMLElement[] = [];

    if (connectable.length >= 2) {
      const bar = document.createElement("div");
      bar.className = "agent-row";
      const summary = document.createElement("span");
      summary.textContent = `${connectable.length} agent apps found on this PC`;
      const connectAll = document.createElement("button");
      connectAll.type = "button";
      connectAll.className = "secondary compact";
      connectAll.textContent = "Connect all detected";
      connectAll.addEventListener("click", async () => {
        ctx.clearError();
        connectAll.disabled = true;
        try {
          await invoke("agent_host_setup", { action: "add-detected" });
          ctx.setStatus("Detected agent apps connected. Choose a permission mode or grant a folder below.");
        } catch (error) {
          ctx.showError(error);
        }
        await refreshHosts();
      });
      bar.append(summary, connectAll);
      nodes.push(bar);
    }

    if (controllableNames.length >= 2) {
      const bulkBar = document.createElement("div");
      bulkBar.className = "agent-bulk-bar";
      const label = document.createElement("span");
      label.textContent = `All ${controllableNames.length} connected/configured agents`;
      const bulkControls = document.createElement("div");
      bulkControls.className = "agent-row-actions";
      const firstMode = effectiveMode(agents.find((a) => a.name === controllableNames[0]));
      const allSame = controllableNames.every(
        (name) => effectiveMode(agents.find((a) => a.name === name)) === firstMode,
      );
      const bulkSelect = modeSelect(firstMode, "Set permission mode for all connected agents", true);
      if (!allSame) {
        const mixed = document.createElement("option");
        mixed.value = "";
        mixed.textContent = "Mixed — apply to all…";
        mixed.selected = true;
        mixed.disabled = true;
        bulkSelect.prepend(mixed);
      }
      bulkSelect.addEventListener("change", async () => {
        const chosen = bulkSelect.value as AgentPermissionMode;
        if (!chosen) return;
        bulkSelect.disabled = true;
        await setModeForAll(controllableNames, chosen);
        bulkSelect.disabled = false;
      });
      bulkControls.append(bulkSelect);
      bulkBar.append(label, bulkControls);
      nodes.push(bulkBar);
    }

    for (const host of detected) {
      const row = document.createElement("div");
      row.className = "agent-row";
      const text = document.createElement("span");
      const connected = host.registered === true && host.owned === true;
      const policy = agents.find((a) => a.name === host.id);
      const mode = effectiveMode(policy);
      const hasGrant = Boolean(policy && (policy.folders.length > 0 || mode === "full_access"));
      text.textContent = connected
        ? `${host.label}: connected`
        : host.registered
          ? `${host.label}: has its own fetchpath entry, left as it is`
          : `${host.label}: ready to connect`;
      row.append(text);

      const controls = document.createElement("div");
      controls.className = "agent-row-actions";
      if (host.registered) {
        const quickMode = modeSelect(mode, `Permission mode for ${host.label}`, true);
        quickMode.addEventListener("change", async () => {
          quickMode.disabled = true;
          await setAgentMode(host.id, quickMode.value as AgentPermissionMode);
          quickMode.disabled = false;
        });
        controls.append(quickMode);
      }
      if (connected && !hasGrant && mode !== "ask") {
        const grantBtn = document.createElement("button");
        grantBtn.type = "button";
        grantBtn.className = "secondary compact";
        grantBtn.textContent = "Grant folder…";
        grantBtn.setAttribute("aria-label", `Grant a folder for ${host.label}`);
        grantBtn.addEventListener("click", async () => {
          await pickAndGrantFolder(host.id);
        });
        controls.append(grantBtn);
      }
      if (connected || !host.registered) {
        const button = document.createElement("button");
        button.type = "button";
        button.className = "secondary compact";
        button.textContent = connected ? "Disconnect" : "Connect";
        button.setAttribute("aria-label", `${connected ? "Disconnect" : "Connect"} ${host.label}`);
        button.addEventListener("click", async () => {
          ctx.clearError();
          button.disabled = true;
          try {
            await invoke("agent_host_setup", { action: connected ? "remove" : "add", host: host.id });
            ctx.setStatus(
              connected
                ? `${host.label} disconnected.`
                : `${host.label} connected. Choose its permission mode or grant a folder below.`,
            );
          } catch (error) {
            ctx.showError(error);
          }
          await refreshHosts();
        });
        controls.append(button);
      }
      if (controls.childElementCount > 0) {
        row.append(controls);
      }
      nodes.push(row);
    }

    if (missing.length > 0) {
      const note = document.createElement("p");
      note.className = "hint field-note";
      note.textContent = `Not found on this PC: ${missing.map((h) => h.label).join(", ")}.`;
      nodes.push(note);
    }

    if (status.universal) {
      const universal = status.universal;
      const details = document.createElement("details");
      details.className = "advanced";
      const summary = document.createElement("summary");
      summary.textContent = "Connect any other MCP agent or install the Agent Skill";
      details.append(summary);

      const help = document.createElement("p");
      help.className = "hint field-note";
      help.textContent =
        "Any MCP host can run fetchpath mcp directly—Fetchpath reads the host's name automatically. Copy a snippet below:";
      details.append(help);

      const copyBar = document.createElement("div");
      copyBar.className = "agent-actions";
      const copyItems: Array<[string, string | undefined, string]> = [
        ["Copy MCP JSON", universal.mcp_json, "MCP JSON snippet copied."],
        ["Copy VS Code JSON", universal.vscode_json, "VS Code MCP JSON copied."],
        ["Copy command", universal.generic_command, "MCP command copied."],
        ["Copy skill install", universal.skill_install, "Agent Skill install command copied."],
      ];
      for (const [label, payload, doneMessage] of copyItems) {
        if (!payload) continue;
        const btn = document.createElement("button");
        btn.type = "button";
        btn.className = "secondary compact";
        btn.textContent = label;
        btn.addEventListener("click", async () => {
          const ok = await copyText(payload);
          if (ok) {
            ctx.setStatus(doneMessage);
          } else {
            ctx.showError("Could not copy to the clipboard.");
          }
        });
        copyBar.append(btn);
      }
      details.append(copyBar);
      nodes.push(details);
    }

    agentHosts.replaceChildren(...nodes);
  }

  async function refresh(): Promise<void> {
    try {
      agents = await invoke<AgentView[]>("list_agents");
      render();
      if (lastStatus) renderHosts(lastStatus);
    } catch (error) {
      ctx.showError(error);
    }
  }

  async function refreshHosts(): Promise<void> {
    try {
      lastStatus = JSON.parse(await invoke<string>("agent_host_setup", { action: "status" })) as AgentSetupStatus;
    } catch {
      // No fetchpath command beside this copy (a development build): nothing to offer.
      lastStatus = null;
      agentHosts.replaceChildren();
      return;
    }
    renderHosts(lastStatus);
  }

  agentsList.addEventListener("change", async (event) => {
    const select = event.target as HTMLSelectElement;
    if (select.dataset.agentMode !== "true") return;
    const name = select.dataset.agent;
    if (!name) return;
    const mode = select.value as AgentPermissionMode;
    await setAgentMode(name, mode, `select[data-agent="${CSS.escape(name)}"][data-agent-mode="true"]`);
  });

  agentsList.addEventListener("click", async (event) => {
    const button = (event.target as HTMLElement).closest<HTMLButtonElement>("button[data-agent-action]");
    if (!button) return;
    const name = button.dataset.agent!;
    const agent = agents.find((candidate) => candidate.name === name);
    if (!agent) return;
    const here = (action: string) => `button[data-agent="${CSS.escape(name)}"][data-agent-action="${action}"]`;
    switch (button.dataset.agentAction) {
      case "add-folder": {
        await pickAndGrantFolder(name, here("add-folder"));
        return;
      }
      case "remove-folder": {
        const index = Number(button.dataset.index);
        const removed = agent.folders[index];
        const folders = agent.folders.filter((_, position) => position !== index);
        await saveAgent(
          { ...agent, folders },
          `${name} can no longer save into ${removed}. Its downloads there wait for your approval.`,
          here("add-folder"),
        );
        return;
      }
      case "save-limits": {
        const size = Number(required<HTMLInputElement>(`agent-size-${name}`).value);
        const rate = Number(required<HTMLInputElement>(`agent-rate-${name}`).value);
        if (!(size >= 1) || !(rate >= 1) || !Number.isInteger(rate)) {
          ctx.showError("Give a size of at least 1 MB and a whole number of downloads an hour.");
          return;
        }
        await saveAgent(
          { ...agent, maxBytes: Math.round(size * MIB), maxNewJobsPerHour: Math.min(rate, 1000) },
          `Limits for ${name} saved.`,
          here("save-limits"),
        );
        return;
      }
      case "revoke": {
        ctx.clearError();
        try {
          agents = await invoke<AgentView[]>("revoke_agent", { name });
          render();
          if (lastStatus) renderHosts(lastStatus);
          ctx.setStatus(`${name} no longer has access. Anything it had not finished waits for your approval.`);
          agentNewName.focus();
        } catch (error) {
          ctx.showError(error);
        }
        return;
      }
    }
  });

  agentAdd.addEventListener("click", async () => {
    const name = agentNewName.value.trim();
    if (!name) {
      ctx.showError("Type the agent's name, as given to fetchpath mcp --agent.");
      agentNewName.focus();
      return;
    }
    if (agents.some((agent) => agent.name === name)) {
      ctx.setStatus(`${name} is already listed.`);
      return;
    }
    agentNewName.value = "";
    await saveAgent(
      {
        name,
        folders: [],
        maxBytes: DEFAULT_AGENT_BYTES,
        maxNewJobsPerHour: DEFAULT_AGENT_PER_HOUR,
        automatic: false,
        permissionMode: "scoped",
      },
      `${name} added. Choose a permission mode or grant a folder below.`,
      `select[data-agent="${CSS.escape(name)}"][data-agent-mode="true"]`,
    );
  });

  agentNewName.addEventListener("keydown", (event) => {
    if (event.key === "Enter") {
      event.preventDefault();
      agentAdd.click();
    }
  });

  return { refresh, refreshHosts };
}
