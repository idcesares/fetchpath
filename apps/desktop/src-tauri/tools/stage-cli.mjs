#!/usr/bin/env node
/**
 * Builds the `fetchpath` command-line tool and stages it where Tauri's
 * `bundle.externalBin` expects it, so the installer ships it beside the app.
 *
 * Tauri looks for `binaries/fetchpath-<target triple>.exe` and installs it as
 * `fetchpath.exe`. The staged copy is a build product and is git-ignored.
 *
 * Run by `beforeBuildCommand`; safe to run by hand:
 *   node apps/desktop/src-tauri/tools/stage-cli.mjs
 */

import { execFileSync } from 'node:child_process';
import { copyFileSync, existsSync, mkdirSync } from 'node:fs';
import { homedir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const srcTauri = fileURLToPath(new URL('../', import.meta.url));
const root = path.resolve(srcTauri, '../../..');
const triple = 'x86_64-pc-windows-msvc';

const installedCargo = path.join(homedir(), '.cargo', 'bin', 'cargo.exe');
const cargo = existsSync(installedCargo) ? installedCargo : 'cargo';

execFileSync(cargo, ['build', '--release', '--locked', '-p', 'fetchpath'], {
  cwd: root,
  stdio: 'inherit',
});
execFileSync(cargo, ['build', '--release', '--locked', '-p', 'fetchpath-torrent', '--bin', 'fetchpath-torrent-helper', '--features', 'helper'], {
  cwd: root,
  stdio: 'inherit',
});

const built = path.join(root, 'target', 'release', 'fetchpath.exe');
const stagedDir = path.join(srcTauri, 'binaries');
mkdirSync(stagedDir, { recursive: true });
const staged = path.join(stagedDir, `fetchpath-${triple}.exe`);
copyFileSync(built, staged);
console.log(`staged ${path.relative(root, staged)}`);
const torrentBuilt = path.join(root, 'target', 'release', 'fetchpath-torrent-helper.exe');
const torrentStaged = path.join(stagedDir, `fetchpath-torrent-helper-${triple}.exe`);
copyFileSync(torrentBuilt, torrentStaged);
console.log(`staged ${path.relative(root, torrentStaged)}`);
