// FP-129: the relay must be built and tested on Linux in CI. The Windows job
// stays. This check is the file, not a VPS run or a certificate.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../../', import.meta.url));
const workflow = readFileSync(path.join(root, '.github/workflows/checks.yml'), 'utf8');

test('the relay is tested and built on Ubuntu without secrets', () => {
  const job = workflow.split(/^ {2}relay-linux:/m)[1]?.split(/^ {2}[a-z0-9-]+:/m)[0];
  assert.ok(job, 'checks.yml needs a relay-linux job');
  assert.match(job, /runs-on:\s*ubuntu-24\.04/);
  assert.match(job, /rustup toolchain install 1\.98\.1/);
  assert.match(job, /cargo test -p fetchpath-relay --features serve --locked/);
  assert.match(job, /cargo build --release -p fetchpath-relay --features serve --locked/);
  assert.doesNotMatch(job, /secrets\./);
  assert.match(workflow, /runs-on:\s*windows-latest/);
});
