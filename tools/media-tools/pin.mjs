#!/usr/bin/env node
/**
 * Records the SHA-256 of each pinned media helper in
 * `apps/desktop/src-tauri/media-tools.json`.
 *
 * This is the only sanctioned way to fill in a `sha256` field. It downloads the
 * artifact *and* the publisher's own checksum file, and writes the digest only
 * when the two agree. A digest taken from the download alone would prove that
 * the bytes arrived intact and nothing more; it would say nothing about who
 * published them, and the application must not present it as though it did.
 *
 * Requires network access, so it is run by a maintainer, not by the build.
 *
 *   node tools/media-tools/pin.mjs          # pin every unpinned entry
 *   node tools/media-tools/pin.mjs --force  # re-pin everything
 *   node tools/media-tools/pin.mjs --check  # verify pins, change nothing
 */

import { createHash } from 'node:crypto';
import { readFile, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../../', import.meta.url));
const manifestPath = path.join(root, 'apps/desktop/src-tauri/media-tools.json');

const force = process.argv.includes('--force');
const checkOnly = process.argv.includes('--check');

/** Downloads a URL into memory, following redirects, with a hard size cap. */
async function fetchBytes(url, limitBytes = 256 * 1024 * 1024) {
  const response = await fetch(url, { redirect: 'follow' });
  if (!response.ok) throw new Error(`${url} returned HTTP ${response.status}`);
  const declared = Number(response.headers.get('content-length') ?? 0);
  if (declared > limitBytes) throw new Error(`${url} declares ${declared} bytes, over the limit`);
  const bytes = Buffer.from(await response.arrayBuffer());
  if (bytes.length > limitBytes) throw new Error(`${url} returned ${bytes.length} bytes, over the limit`);
  return bytes;
}

const sha256 = (bytes) => createHash('sha256').update(bytes).digest('hex');

/**
 * Pulls the digest for one file out of a publisher checksum document.
 *
 * Handles both shapes in use: a multi-line `SHA2-256SUMS` listing many files,
 * and a single-file `.sha256` containing one digest with or without a filename.
 */
function digestFromChecksumFile(text, artifactFilename) {
  const lines = text.split(/\r?\n/).map((line) => line.trim()).filter(Boolean);
  for (const line of lines) {
    const match = line.match(/^([0-9a-fA-F]{64})\s+\*?(.+)$/);
    if (match && path.basename(match[2]) === artifactFilename) return match[1].toLowerCase();
  }
  // A single-file checksum document often carries the digest on its own.
  if (lines.length === 1) {
    const bare = lines[0].match(/^([0-9a-fA-F]{64})$/);
    if (bare) return bare[1].toLowerCase();
  }
  throw new Error(`no SHA-256 for ${artifactFilename} in the publisher checksum file`);
}

async function pin(tool) {
  const artifactFilename = path.basename(new URL(tool.url).pathname);
  process.stdout.write(`${tool.name} ${tool.version}\n  artifact ${tool.url}\n`);

  const [artifact, checksumDocument] = await Promise.all([
    fetchBytes(tool.url),
    fetchBytes(tool.checksumSource, 1024 * 1024).then((bytes) => bytes.toString('utf8')),
  ]);

  const observed = sha256(artifact);
  const published = digestFromChecksumFile(checksumDocument, artifactFilename);

  if (observed !== published) {
    throw new Error(
      `${tool.name}: the download does not match the publisher checksum.\n` +
        `  published: ${published}\n  received:  ${observed}\n` +
        '  Nothing was written. Do not pin this artifact.',
    );
  }
  process.stdout.write(`  verified against ${tool.checksumSource}\n  sha256   ${observed}\n`);
  return observed;
}

async function main() {
  const raw = await readFile(manifestPath, 'utf8');
  const manifest = JSON.parse(raw);
  let changed = false;
  const failures = [];

  for (const tool of manifest.tools) {
    if (tool.sha256 && !force && !checkOnly) {
      process.stdout.write(`${tool.name} ${tool.version}\n  already pinned, skipping\n`);
      continue;
    }
    try {
      const digest = await pin(tool);
      if (checkOnly) {
        if (tool.sha256 && tool.sha256.toLowerCase() !== digest) {
          failures.push(`${tool.name}: recorded ${tool.sha256} but the publisher now serves ${digest}`);
        }
        continue;
      }
      if (tool.sha256 !== digest) {
        tool.sha256 = digest;
        changed = true;
      }
    } catch (error) {
      failures.push(error.message);
    }
  }

  if (changed) {
    await writeFile(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`);
    process.stdout.write(`\nUpdated ${path.relative(root, manifestPath)}\n`);
  }
  if (failures.length) {
    throw new Error(`\n${failures.join('\n')}`);
  }
  process.stdout.write('\nAll pinned artifacts match their publisher checksums.\n');
}

main().catch((error) => {
  console.error(error.message);
  process.exitCode = 1;
});
