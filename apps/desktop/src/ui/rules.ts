import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { required } from "./dom";

/** One smart rule, as the engine reports it. */
export interface RuleView {
  id: number;
  label: string;
  when: string;
  then: string;
}

export interface RuleAdvice {
  matched: string | null;
  folder: string | null;
  needsChecksum: boolean;
  lines: string[];
}

export interface RulesPanelContext {
  showError(error: unknown): void;
  clearError(): void;
  setStatus(message: string): void;
}

export interface RulesPanel {
  refresh(): Promise<void>;
}

const MIB = 1024 * 1024;

function listOf(value: string): string[] {
  return value.split(",").map((item) => item.trim().replace(/^\./, "")).filter(Boolean);
}

function megabytes(input: HTMLInputElement): number | undefined {
  const value = input.value.trim();
  if (!value) return undefined;
  return Math.round(Number(value) * MIB);
}

/** Smart rules editor and tester inside Settings (FP-075). */
export function createRulesPanel(ctx: RulesPanelContext): RulesPanel {
  const rulesList = required<HTMLOListElement>("rules-list");
  const ruleName = required<HTMLInputElement>("rule-name");
  const ruleDomains = required<HTMLInputElement>("rule-domains");
  const ruleTypes = required<HTMLInputElement>("rule-types");
  const ruleMinSize = required<HTMLInputElement>("rule-min-size");
  const ruleMaxSize = required<HTMLInputElement>("rule-max-size");
  const ruleFolderInput = required<HTMLInputElement>("rule-folder");
  const ruleChooseFolder = required<HTMLButtonElement>("rule-choose-folder");
  const ruleQuality = required<HTMLSelectElement>("rule-quality");
  const ruleConnections = required<HTMLSelectElement>("rule-connections");
  const ruleChecksum = required<HTMLInputElement>("rule-checksum");
  const ruleSave = required<HTMLButtonElement>("rule-save");
  const ruleTestLink = required<HTMLInputElement>("rule-test-link");
  const ruleTest = required<HTMLButtonElement>("rule-test");
  const ruleTestResult = required<HTMLUListElement>("rule-test-result");

  let rules: RuleView[] = [];

  function render(): void {
    rulesList.replaceChildren();
    if (!rules.length) {
      const empty = document.createElement("li");
      empty.className = "field-note";
      empty.textContent = "No rules yet. Every download goes where you choose, or to the default save folder.";
      rulesList.append(empty);
      return;
    }
    for (const rule of rules) {
      const item = document.createElement("li");
      const row = document.createElement("div");
      row.className = "rule-row";
      const text = document.createElement("div");
      const heading = document.createElement("h3");
      heading.id = `rule-${rule.id}`;
      heading.textContent = rule.label;
      const summary = document.createElement("p");
      summary.className = "hint field-note";
      summary.textContent = `${rule.when} → ${rule.then}`;
      text.append(heading, summary);
      const remove = document.createElement("button");
      remove.type = "button";
      remove.className = "secondary danger";
      remove.textContent = "Remove";
      remove.dataset.ruleId = String(rule.id);
      remove.setAttribute("aria-label", `Remove ${rule.label}`);
      row.append(text, remove);
      item.append(row);
      rulesList.append(item);
    }
  }

  async function refresh(): Promise<void> {
    try {
      rules = await invoke<RuleView[]>("list_rules");
      render();
    } catch (error) {
      ctx.showError(error);
    }
  }

  rulesList.addEventListener("click", async (event) => {
    const button = (event.target as HTMLElement).closest<HTMLButtonElement>("button[data-rule-id]");
    if (!button) return;
    const label = rules.find((rule) => String(rule.id) === button.dataset.ruleId)?.label ?? "The rule";
    ctx.clearError();
    try {
      rules = await invoke<RuleView[]>("remove_rule", { ruleId: Number(button.dataset.ruleId) });
      render();
      ctx.setStatus(`${label} removed.`);
    } catch (error) {
      ctx.showError(error);
    }
    (rulesList.querySelector<HTMLElement>("button[data-rule-id]") ?? ruleTestLink).focus();
  });

  ruleChooseFolder.addEventListener("click", async () => {
    const selected = await open({ directory: true, title: "Choose a folder for this rule" }).catch(() => null);
    if (typeof selected === "string") ruleFolderInput.value = selected;
    ruleFolderInput.focus();
  });

  ruleSave.addEventListener("click", async () => {
    ctx.clearError();
    const connections = ruleConnections.value ? Number(ruleConnections.value) : undefined;
    const rule = {
      name: ruleName.value.trim() || undefined,
      when: {
        domains: listOf(ruleDomains.value.toLowerCase()),
        file_types: listOf(ruleTypes.value.toLowerCase()),
        min_size_bytes: megabytes(ruleMinSize),
        max_size_bytes: megabytes(ruleMaxSize),
      },
      then: {
        folder: ruleFolderInput.value.trim() || undefined,
        media_quality: ruleQuality.value || undefined,
        require_checksum: ruleChecksum.checked,
        max_connections: connections,
      },
    };
    try {
      rules = await invoke<RuleView[]>("add_rule", { rule });
      render();
      for (const input of [ruleName, ruleDomains, ruleTypes, ruleMinSize, ruleMaxSize, ruleFolderInput]) input.value = "";
      ruleQuality.value = "";
      ruleConnections.value = "";
      ruleChecksum.checked = false;
      ctx.setStatus(`${rules[rules.length - 1]?.label ?? "The rule"} added.`);
    } catch (error) {
      ctx.showError(error);
    }
  });

  ruleTest.addEventListener("click", async () => {
    ctx.clearError();
    ruleTestResult.replaceChildren();
    const url = ruleTestLink.value.trim();
    if (!url) return;
    try {
      const advice = await invoke<RuleAdvice>("inspect_rules", { url });
      for (const line of advice.lines) {
        const item = document.createElement("li");
        item.textContent = line.trim();
        ruleTestResult.append(item);
      }
    } catch (error) {
      ctx.showError(error);
    }
  });

  ruleTestLink.addEventListener("keydown", (event) => {
    if (event.key === "Enter") {
      event.preventDefault();
      ruleTest.click();
    }
  });

  return { refresh };
}
