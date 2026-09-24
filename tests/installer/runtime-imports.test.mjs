// Shipped executables must run on a clean Windows install (FP-044).
// Regression: fetchpath.exe imported VCRUNTIME140.dll, which a fresh Windows
// image (Windows Sandbox included) does not have, so `fetchpath --help` exited
// silently after install. The Universal CRT (api-ms-win-crt-*) ships with every
// supported Windows and is allowed.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

// The command line links the runtime statically through apps/cli/build.rs, so
// every build of it is checked, including the copy staged for the installer.
// The desktop app and browser host get the same from tauri-build, but only
// under `tauri build` (STATIC_VCRUNTIME), so a plain `cargo build` of them is
// not representative and they are not checked here.
const shipped = [
  fileURLToPath(new URL('../../target/release/fetchpath.exe', import.meta.url)),
  fileURLToPath(new URL('../../apps/desktop/src-tauri/binaries/fetchpath-x86_64-pc-windows-msvc.exe', import.meta.url)),
];

/** Names of the DLLs a PE32+ image imports. */
function importedDlls(file) {
  const image = readFileSync(file);
  const pe = image.readUInt32LE(0x3c);
  assert.equal(image.toString('latin1', pe, pe + 4), 'PE\0\0', `${file} is not a PE image`);
  const sections = image.readUInt16LE(pe + 6);
  const optional = pe + 24;
  assert.equal(image.readUInt16LE(optional), 0x20b, 'expected PE32+');
  const importRva = image.readUInt32LE(optional + 120);
  const table = optional + image.readUInt16LE(pe + 20);
  const offset = (rva) => {
    for (let index = 0; index < sections; index += 1) {
      const header = table + 40 * index;
      const size = image.readUInt32LE(header + 8);
      const address = image.readUInt32LE(header + 12);
      if (rva >= address && rva < address + size) return rva - address + image.readUInt32LE(header + 20);
    }
    throw new Error(`RVA ${rva} is outside every section`);
  };
  const names = [];
  for (let entry = offset(importRva); ; entry += 20) {
    const nameRva = image.readUInt32LE(entry + 12);
    if (nameRva === 0) break;
    const start = offset(nameRva);
    names.push(image.toString('latin1', start, image.indexOf(0, start)));
  }
  return names;
}

for (const file of shipped) {
  test(`${file.split(/[\\/]/).pop()} needs no Visual C++ redistributable`, { skip: !existsSync(file) && 'not built' }, () => {
    const dlls = importedDlls(file);
    assert.ok(dlls.some((dll) => /^kernel32\.dll$/i.test(dll)), 'import table was not read');
    const redistributable = dlls.filter((dll) => /^(vcruntime|msvcp)\d+.*\.dll$/i.test(dll));
    assert.deepEqual(redistributable, []);
  });
}
