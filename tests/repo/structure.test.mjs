// Keeps the repository's shape honest: every tracked component is named in the
// repository map, every relative documentation link resolves, the Cargo
// workspace lists only directories that exist, and nothing tracked lives in the
// ignored scratch areas.
//
// Retired spikes and moved documents are the usual way these drift; each check
// names the offending path so the fix is obvious.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, existsSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../../', import.meta.url));
const read = (relative) => readFileSync(path.join(root, relative), 'utf8');

// Tracked files plus untracked files that are not ignored, so a new component
// is checked before its first commit.
const files = execFileSync('git', ['ls-files', '--cached', '--others', '--exclude-standard'], { cwd: root, encoding: 'utf8' })
  .split('\n')
  .filter(Boolean)
  .filter((file) => existsSync(path.join(root, file)))
  // Per-user agent settings are local configuration, not a component.
  .filter((file) => !file.startsWith('.claude/'));

// Directories whose children are separate components rather than one area.
const grouped = new Set(['adapters', 'apps', 'crates', 'docs', 'extensions', 'tests', 'tools']);

function components() {
  const found = new Set();
  for (const file of files) {
    const parts = file.split('/');
    if (parts.length === 1) continue;
    found.add(grouped.has(parts[0]) ? `${parts[0]}/${parts[1]}` : parts[0]);
  }
  return [...found].sort();
}

test('every tracked component is named in the repository map', () => {
  const map = read('docs/architecture/REPOSITORY.md');
  const missing = components().filter((component) => !map.includes(`\`${component}\``));
  assert.deepEqual(missing, [], 'add these to docs/architecture/REPOSITORY.md or remove them');
});

test('relative links in Markdown resolve', () => {
  const broken = [];
  let checked = 0;
  for (const file of files.filter((name) => name.endsWith('.md'))) {
    const text = read(file).replace(/```[\s\S]*?```/g, '');
    for (const [, raw] of text.matchAll(/\[[^\]]*\]\(([^)\s]+)\)/g)) {
      const target = decodeURIComponent(raw.split('#')[0]);
      if (!target || /^[a-z][a-z0-9+.-]*:/i.test(target)) continue;
      checked += 1;
      if (!existsSync(path.resolve(root, path.dirname(file), target))) broken.push(`${file} -> ${raw}`);
    }
  }
  assert.ok(checked > 0, 'expected to check at least one link');
  assert.deepEqual(broken, []);
});

test('the Cargo workspace names only existing packages', () => {
  const manifest = read('Cargo.toml');
  const list = (key) => {
    const match = manifest.match(new RegExp(`^${key}\\s*=\\s*\\[([^\\]]*)\\]`, 'm'));
    return match ? [...match[1].matchAll(/"([^"]+)"/g)].map(([, entry]) => entry) : [];
  };
  const members = list('members');
  assert.ok(members.length, 'expected workspace members');
  for (const member of members) {
    assert.ok(existsSync(path.join(root, member, 'Cargo.toml')), `workspace member ${member} has no Cargo.toml`);
  }
  for (const excluded of list('exclude')) {
    assert.ok(existsSync(path.join(root, excluded)), `workspace excludes ${excluded}, which does not exist`);
  }
});

test('nothing is tracked in the ignored scratch and build areas', () => {
  const tracked = execFileSync('git', ['ls-files', '--', 'work', 'target'], { cwd: root, encoding: 'utf8' }).trim();
  assert.equal(tracked, '');
});

test('package scripts run files that exist', () => {
  const { scripts } = JSON.parse(read('package.json'));
  for (const [name, command] of Object.entries(scripts)) {
    for (const [, script] of command.matchAll(/node\s+([\w./-]+\.m?js)/g)) {
      assert.ok(existsSync(path.join(root, script)), `script ${name} runs missing ${script}`);
    }
  }
});

// Two owners of one queue corrupt it (engine platform design §9). The engine
// is the only owner; the desktop app is its client and must never build a
// session of its own or take the engine's lock (FP-055). The browser host
// shares the crate and may use the session's inbox types, nothing more.
test('the desktop app never owns the queue', () => {
  const sources = files.filter((file) => file.startsWith('apps/desktop/src-tauri/src/') && file.endsWith('.rs'));
  assert.ok(sources.length > 0);
  for (const file of sources) {
    const text = read(file);
    for (const [pattern, what] of [
      [/\bSession::|fetchpath_session::(Session\b|engine\b|\{[^}]*\bSession\b)/, 'a session'],
      [/fetchpath_session::engine|\bEngine::new\b/, 'an engine'],
      [/"instance\.lock"/, "the engine's lock"],
    ]) {
      assert.ok(!pattern.test(text), `${file} builds or claims ${what}`);
    }
  }
});
