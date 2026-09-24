import { readFile, access } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

export const root = fileURLToPath(new URL('../', import.meta.url));
// P0 blocks the current phase, P1 is a headline outcome of it, P2 is valuable
// next, P3 is later or research. Every unfinished task carries one.
export const priorities = ['P0', 'P1', 'P2', 'P3'];
// 'strong' marks integrity, persistence, unsafe/FFI, credential and agent-policy
// work that needs an independent strong-model review before it is done.
export const reviews = ['strong', 'standard'];
export function validateGraph(backlog) {
  const errors = [];
  if (backlog?.schemaVersion !== 1 || !Array.isArray(backlog.tasks)) return ['Invalid backlog schema'];
  const ids = new Map();
  for (const task of backlog.tasks) {
    if (!/^FP-\d{3}$/.test(task.id)) errors.push(`Invalid id: ${task.id}`);
    if (ids.has(task.id)) errors.push(`Duplicate id: ${task.id}`);
    ids.set(task.id, task);
    if (!['todo', 'in_progress', 'blocked', 'done'].includes(task.status)) errors.push(`${task.id}: invalid status`);
    if (!task.title || !task.acceptance || !task.verification) errors.push(`${task.id}: missing contract`);
    if (!Array.isArray(task.dependsOn) || !Array.isArray(task.files) || !Array.isArray(task.evidence)) errors.push(`${task.id}: invalid arrays`);
    if (!Array.isArray(task.acceptanceIds) || !task.acceptanceIds.length || task.acceptanceIds.some(id => !/^A(0[1-9]|1[0-3])$/.test(id))) errors.push(`${task.id}: invalid acceptance IDs`);
    if (task.status !== 'done' && !priorities.includes(task.priority)) errors.push(`${task.id}: unfinished task needs priority P0-P3`);
    if (task.review !== undefined && !reviews.includes(task.review)) errors.push(`${task.id}: invalid review`);
    if (task.status === 'in_progress' && !task.owner) errors.push(`${task.id}: active task needs owner`);
    if (task.status === 'blocked' && !task.blockedReason) errors.push(`${task.id}: blocked task needs reason`);
    if (task.status === 'done' && !task.evidence?.length) errors.push(`${task.id}: done task needs evidence`);
  }
  const visiting = new Set();
  const visited = new Set();
  function visit(id) {
    if (visiting.has(id)) { errors.push(`${id}: dependency cycle`); return; }
    if (visited.has(id)) return;
    visiting.add(id);
    const task = ids.get(id);
    for (const dep of Array.isArray(task?.dependsOn) ? task.dependsOn : []) {
      if (!ids.has(dep)) errors.push(`${id}: missing dependency ${dep}`);
      else {
        if (['done', 'in_progress'].includes(task.status) && ids.get(dep).status !== 'done') errors.push(`${id}: unfinished dependency ${dep}`);
        visit(dep);
      }
    }
    visiting.delete(id);
    visited.add(id);
  }
  for (const id of ids.keys()) visit(id);
  return errors;
}

export function readyTasks(backlog) {
  const done = new Set(backlog.tasks.filter(t => t.status === 'done').map(t => t.id));
  return backlog.tasks
    .filter(t => t.status === 'todo' && t.dependsOn.every(id => done.has(id)))
    .sort((a, b) => priorities.indexOf(a.priority) - priorities.indexOf(b.priority) || a.id.localeCompare(b.id));
}

export async function validateEvidence(backlog, base = root) {
  const errors = [];
  for (const task of backlog.tasks) for (const evidence of task.evidence) {
    const target = path.resolve(base, evidence);
    const relative = path.relative(base, target);
    if (relative.startsWith('..') || path.isAbsolute(relative)) { errors.push(`${task.id}: evidence escapes repository`); continue; }
    try { await access(target); } catch { errors.push(`${task.id}: missing evidence ${evidence}`); }
  }
  return errors;
}

async function main() {
  const backlog = JSON.parse(await readFile(path.join(root, 'docs/tasks/backlog.json'), 'utf8'));
  const errors = validateGraph(backlog);
  if (errors.length) throw new Error(errors.join('\n'));
  const [command = 'next', id] = process.argv.slice(2);
  if (command === 'check') {
    const missing = await validateEvidence(backlog);
    if (missing.length) throw new Error(missing.join('\n'));
    console.log(`PASS: ${backlog.tasks.length} tasks; dependencies, states, contracts and evidence valid.`);
  } else if (command === 'show') {
    const task = backlog.tasks.find(t => t.id === id);
    if (!task) throw new Error(`Unknown task: ${id}`);
    console.log(JSON.stringify(task, null, 2));
  } else if (command === 'list' || command === 'next') {
    const ready = new Set(readyTasks(backlog).map(t => t.id));
    // `next` shows active work first: an in_progress task often gates the ready list.
    const active = command === 'next' ? backlog.tasks.filter(t => t.status === 'in_progress') : [];
    for (const task of command === 'next' ? [...active, ...readyTasks(backlog)] : backlog.tasks) console.log(`${task.id} [${ready.has(task.id) ? 'ready' : task.status}] ${task.priority ?? '--'} ${task.milestone} ${task.title}${task.review === 'strong' ? ' (strong review)' : ''}`);
  } else throw new Error('Usage: node tools/tasks.mjs [next|list|show FP-XXX|check]');
}
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch(error => { console.error(error.message); process.exitCode = 1; });
}
