#!/usr/bin/env node
/**
 * Writes SHA256SUMS.txt next to the installer this build produced, in the
 * `<digest> *<file>` form `Get-FileHash` output can be compared against and
 * `sha256sum -c` can check. Publish it beside the installer.
 *
 * This is an integrity record for the download, not a publisher signature:
 * whoever can replace the installer on the release page can replace this file
 * too. Code signing is what would add publisher authenticity.
 *
 * Run by the `release` script after `tauri build`.
 */

import { createHash } from 'node:crypto';
import { readFileSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const srcTauri = fileURLToPath(new URL('../', import.meta.url));
const root = path.resolve(srcTauri, '../../..');
const { version } = JSON.parse(readFileSync(path.join(srcTauri, 'tauri.conf.json'), 'utf8'));
const bundle = path.join(root, 'target', 'release', 'bundle', 'nsis');
const installer = `Fetchpath_${version}_x64-setup.exe`;

const digest = createHash('sha256').update(readFileSync(path.join(bundle, installer))).digest('hex');
writeFileSync(path.join(bundle, 'SHA256SUMS.txt'), `${digest} *${installer}\n`);
console.log(`${digest}  ${installer}`);
