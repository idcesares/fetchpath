// FP-130: the relay unit only uses flags the binary already accepts, and it
// does not enable MemoryDenyWriteExecute. systemd-analyze is a separate run.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../../', import.meta.url));
const read = (relative) => readFileSync(path.join(root, relative), 'utf8');

test('the relay unit matches the binary and stays short of an unproven sandbox', () => {
  const unit = read('crates/fetchpath-relay/fetchpath-relay.service');
  const source = read('crates/fetchpath-relay/src/main.rs');
  const exec = unit.match(/^ExecStart=(.*)$/m)?.[1];
  assert.ok(exec, 'ExecStart is missing');
  const flags = [...exec.matchAll(/--[a-z0-9-]+/g)].map((match) => match[0]);
  assert.ok(flags.length > 0, 'ExecStart names no flags');
  for (const flag of flags) assert.ok(source.includes(flag), `${flag} is not a relay flag`);
  for (const line of [
    'User=fetchpath-relay',
    'Group=fetchpath-relay',
    'EnvironmentFile=/etc/fetchpath-relay.env',
    'StateDirectory=fetchpath-relay',
    'StateDirectoryMode=0700',
    'AmbientCapabilities=CAP_NET_BIND_SERVICE',
    'CapabilityBoundingSet=CAP_NET_BIND_SERVICE',
    'NoNewPrivileges=yes',
    'ProtectSystem=strict',
    'ProtectHome=yes',
    'PrivateTmp=yes',
    'RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX',
  ]) assert.ok(unit.includes(line), `missing ${line}`);
  assert.equal(unit.includes('MemoryDenyWriteExecute'), false);
  assert.match(read('.github/workflows/checks.yml'), /systemd-analyze verify/);
});
