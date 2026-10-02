#!/usr/bin/env node
// Copies design/tokens.css into extensions/browser/tokens.css (FP-095). The
// extension cannot import across the repository, so it ships a copy; this is
// the only way that copy changes. `--check` fails when it has drifted.
import { copyFileSync, readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const source = fileURLToPath(new URL('../design/tokens.css', import.meta.url));
const target = fileURLToPath(new URL('../extensions/browser/tokens.css', import.meta.url));

if (process.argv.includes('--check')) {
  if (readFileSync(source, 'utf8') !== readFileSync(target, 'utf8')) {
    console.error('extensions/browser/tokens.css differs from design/tokens.css; run node tools/sync-design-tokens.mjs');
    process.exit(1);
  }
} else {
  copyFileSync(source, target);
  console.log('Copied design/tokens.css to extensions/browser/tokens.css');
}
