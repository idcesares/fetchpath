import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { required } from "./dom";
import { formatBytes } from "./format";

/** One agent's access, as the engine keeps it. */
export interface AgentView {
  name: string;
  folders: string[];
  maxBytes: number;
  maxNewJobsPerHour: number;
  /** Inside its folders, nothing it downloads waits for approval. */
  automatic: boolean;
}

interface AgentHostState {
  available?: boolean;
  registered?: boolean;
  owned?: boolean;
  connection?: string;
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

const AGENT_HOSTS = [
  { key: "codex", id: "codex", label: "Codex" },
  { key: "claude_code", id: "claude-code", label: "Claude Code" },
] as const;

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

/** AI agent access policies (FP-066) and Codex/Claude Code host registration. */
export function createAgentsPanel(ctx: AgentsPanelContext): AgentsPanel {
  const agentsList = required<HTMLDivElement>("agents-list");
  const agentNewName = required<HTMLInputElement>("agent-new-name");
  const agentAdd = required<HTMLButtonElement>("agent-add");
  const agentHosts = required<HTMLElement>("agent-hosts");

  let agents: AgentView[] = [];

  async function saveAgent(agent: AgentView, message: string, focus: string): Promise<void> {
    ctx.clearError();
    try {
      agents = await invoke<AgentView[]>("set_agent", {
        name: agent.name,
        folders: agent.folders,
        maxBytes: agent.maxBytes,
        maxNewJobsPerHour: agent.maxNewJobsPerHour,
        automatic: agent.automatic,
      });
      render();
      ctx.setStatus(message);
    } catch (error) {
      ctx.showError(error);
    }
    agentsList.querySelector<HTMLElement>(focus)?.focus();
  }

  function render(): void {
    agentsList.replaceChildren();
    if (!agents.length) {
      const empty = document.createElement("p");
      empty.className = "field-note";
      empty.textContent = "No agent has access yet. Every request from an agent waits for you.";
      agentsList.append(empty);
      return;
    }
    for (const agent of agents) {
      const card = document.createElement("section");
      card.className = "agent-card";
      card.setAttribute("aria-labelledby", `agent-${agent.name}`);
      const heading = document.createElement("h3");
      heading.id = `agent-${agent.name}`;
      heading.textContent = agent.name;
      const summary = document.createElement("p");
      summary.className = "hint field-note";
      const where = agent.folders.length === 1 ? "this folder" : "these folders";
      summary.textContent = !agent.folders.length
        ? "No folders yet: everything it asks for waits for you."
        : agent.automatic
          ? `Automatic: saves into ${where} without asking, whatever the size or how many. Anything elsewhere still waits for you.`
          : `Saves into ${where} without asking, up to ${formatBytes(agent.maxBytes)} a download and ${agent.maxNewJobsPerHour} downloads an hour.`;
      card.append(heading, summary);

      const automaticLabel = document.createElement("label");
      automaticLabel.className = "check";
      const automatic = document.createElement("input");
      automatic.type = "checkbox";
      automatic.checked = agent.automatic;
      automatic.dataset.agent = agent.name;
      automatic.dataset.agentToggle = "automatic";
      const automaticText = document.createElement("span");
      automaticText.textContent =
        "Automatic: inside its folders, never ask about size, how many an hour, or torrent peers";
      automaticLabel.append(automatic, automaticText);
      card.append(automaticLabel);

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

      const actions = document.createElement("div");
      actions.className = "agent-actions";
      actions.append(
        agentButton("Add folder…", agent.name, "add-folder", `Add a folder for ${agent.name}`),
        agentButton("Revoke access", agent.name, "revoke", `Revoke access for ${agent.name}`, true),
      );
      card.append(actions);
      agentsList.append(card);
    }
  }

  async function refresh(): Promise<void> {
    try {
      agents = await invoke<AgentView[]>("list_agents");
      render();
    } catch (error) {
      ctx.showError(error);
    }
  }

  async function refreshHosts(): Promise<void> {
    let status: Record<string, AgentHostState> = {};
    try {
      status = JSON.parse(await invoke<string>("agent_host_setup", { action: "status" }));
    } catch {
      // No fetchpath command beside this copy (a development build): nothing to offer.
      agentHosts.replaceChildren();
      return;
    }
    agentHosts.replaceChildren(
      ...AGENT_HOSTS.map((host) => {
        const state = status[host.key] ?? {};
        const row = document.createElement("div");
        row.className = "agent-row";
        const text = document.createElement("span");
        const connected = state.registered === true && state.owned === true;
        text.textContent = !state.available
          ? `${host.label}: not found on this PC`
          : connected
            ? `${host.label}: connected. Restart it to use Fetchpath.`
            : state.registered
              ? `${host.label}: has its own fetchpath entry, left as it is`
              : `${host.label}: not connected`;
        row.append(text);
        if (state.available && (connected || !state.registered)) {
          const button = document.createElement("button");
          button.type = "button";
          button.className = "secondary compact";
          button.textContent = connected ? "Disconnect" : "Connect";
          button.addEventListener("click", async () => {
            ctx.clearError();
            button.disabled = true;
            try {
              await invoke("agent_host_setup", { action: connected ? "remove" : "add", host: host.id });
              ctx.setStatus(
                connected
                  ? `${host.label} disconnected.`
                  : `${host.label} connected. It can download only into folders you grant.`,
              );
            } catch (error) {
              ctx.showError(error);
            }
            await refreshHosts();
          });
          row.append(button);
        }
        return row;
      }),
    );
  }

  agentsList.addEventListener("change", async (event) => {
    const toggle = event.target as HTMLInputElement;
    if (toggle.dataset.agentToggle !== "automatic") return;
    const agent = agents.find((candidate) => candidate.name === toggle.dataset.agent);
    if (!agent) return;
    await saveAgent(
      { ...agent, automatic: toggle.checked },
      toggle.checked
        ? `${agent.name} is automatic: inside its folders its downloads no longer wait for you.`
        : `${agent.name} is no longer automatic: its size and hourly limits apply again.`,
      `input[data-agent="${CSS.escape(agent.name)}"][data-agent-toggle="automatic"]`,
    );
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
        ctx.clearError();
        try {
          const selected = await open({ directory: true, title: `Choose a folder ${name} may save into` });
          if (typeof selected !== "string") return;
          if (agent.folders.some((folder) => folder.toLowerCase() === selected.toLowerCase())) {
            ctx.setStatus(`${name} can already save into ${selected}.`);
            return;
          }
          await saveAgent({ ...agent, folders: [...agent.folders, selected] }, `${name} can now save into ${selected}.`, here("add-folder"));
        } catch (error) {
          ctx.showError(error);
        }
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
      { name, folders: [], maxBytes: DEFAULT_AGENT_BYTES, maxNewJobsPerHour: DEFAULT_AGENT_PER_HOUR, automatic: false },
      `${name} added. Give it a folder so its downloads there start without asking.`,
      `button[data-agent="${CSS.escape(name)}"][data-agent-action="add-folder"]`,
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
