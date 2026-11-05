'use strict';
// The share page (FP-117). Served inline with a CSP that allows exactly this
// text, so keep it free of the strings the listener fills in. It reads the
// key from the URL fragment (which the browser never sends), fetches the
// encrypted stream from the same address and decrypts it here; the server
// never sees the key or the file name. The framing is docs/development/
// SHARE-RELAY.md; the Rust side is crates/fetchpath-session/src/share/frame.rs.
(function () {
  const HEADER_LEN = 22;
  const TAG_LEN = 16;
  const MIN_LOG2 = 16;
  const MAX_LOG2 = 22;
  const MAX_NAME = 1024;
  const INFO = new TextEncoder().encode('fetchpath-share-v1');

  class ShareError extends Error {}

  // The 43-character fragment is the 32-byte key in base64url.
  function keyFromFragment(text) {
    if (!/^[A-Za-z0-9_-]{43}$/.test(text)) throw new ShareError('key');
    const binary = atob(text.replace(/-/g, '+').replace(/_/g, '/') + '=');
    const key = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i += 1) key[i] = binary.charCodeAt(i);
    if (key.length !== 32) throw new ShareError('key');
    return key;
  }

  function join(a, b) {
    if (a.length === 0) return b;
    const out = new Uint8Array(a.length + b.length);
    out.set(a, 0);
    out.set(b, a.length);
    return out;
  }

  // Feed it the encrypted stream in any pieces; `push` returns the plain
  // bytes of every chunk that is now certain not to be the last, `finish`
  // returns the last. A chunk of full size is held back until a byte follows
  // it, so the last-chunk flag in its nonce decides whether the stream is
  // whole: truncation, trailing bytes and reordering all fail to open.
  async function createDecryptor(keyBytes) {
    const subtle = globalThis.crypto.subtle;
    let buffer = new Uint8Array(0);
    let context = null;
    let index = 0;
    let finished = false;

    async function setUp(header) {
      const magic = header[0] === 0x46 && header[1] === 0x50 && header[2] === 0x53 && header[3] === 0x31;
      if (!magic || header[4] !== 1 || header[5] < MIN_LOG2 || header[5] > MAX_LOG2) {
        throw new ShareError('format');
      }
      const base = await subtle.importKey('raw', keyBytes, 'HKDF', false, ['deriveBits']);
      const okm = new Uint8Array(await subtle.deriveBits(
        { name: 'HKDF', hash: 'SHA-256', salt: header.slice(6), info: INFO }, base, 39 * 8));
      const key = await subtle.importKey('raw', okm.slice(0, 32), 'AES-GCM', false, ['decrypt']);
      context = { key, prefix: okm.slice(32), header: header.slice(), sealed: (1 << header[5]) + TAG_LEN };
    }

    async function open(last, sealed) {
      if (index > 0xffffffff) throw new ShareError('damaged');
      const iv = new Uint8Array(12);
      iv.set(context.prefix, 0);
      new DataView(iv.buffer).setUint32(7, index, false);
      iv[11] = last ? 1 : 0;
      try {
        const plain = await subtle.decrypt(
          { name: 'AES-GCM', iv, additionalData: context.header, tagLength: 128 }, context.key, sealed);
        index += 1;
        return new Uint8Array(plain);
      } catch (error) {
        throw new ShareError('damaged');
      }
    }

    return {
      async push(bytes) {
        if (finished) throw new ShareError('damaged');
        buffer = join(buffer, bytes);
        const out = [];
        if (context === null) {
          if (buffer.length < HEADER_LEN) return out;
          await setUp(buffer.subarray(0, HEADER_LEN));
          buffer = buffer.slice(HEADER_LEN);
        }
        while (buffer.length > context.sealed) {
          out.push(await open(false, buffer.subarray(0, context.sealed)));
          buffer = buffer.slice(context.sealed);
        }
        return out;
      },
      async finish() {
        if (finished || context === null || buffer.length < TAG_LEN) throw new ShareError('damaged');
        finished = true;
        return [await open(true, buffer)];
      },
      // Where to ask the server to continue after a dropped connection: the
      // start of the first chunk not opened yet. Drops what was buffered.
      resumeAt() {
        if (context === null) return 0;
        buffer = new Uint8Array(0);
        return HEADER_LEN + index * context.sealed;
      },
    };
  }

  // `name length (u16 BE) || name` opens the plain stream; the rest is the file.
  function createSplitter() {
    let head = new Uint8Array(0);
    let name = null;
    return {
      feed(piece) {
        if (name !== null) return piece;
        head = join(head, piece);
        if (head.length < 2) return new Uint8Array(0);
        const length = (head[0] << 8) | head[1];
        if (length === 0 || length > MAX_NAME) throw new ShareError('damaged');
        if (head.length < 2 + length) return new Uint8Array(0);
        try {
          name = new TextDecoder('utf-8', { fatal: true }).decode(head.subarray(2, 2 + length));
        } catch (error) {
          throw new ShareError('damaged');
        }
        return head.subarray(2 + length);
      },
      name() {
        return name;
      },
    };
  }

  // What the browser is asked to save the file as: no path, no characters a
  // file system refuses, never empty.
  function safeName(name) {
    const cleaned = Array.from(name)
      .map((c) => (/[\\/:*?"<>|\u0000-\u001f\u007f‪-‮⁦-⁩]/.test(c) ? '_' : c))
      .join('')
      .replace(/^[. ]+|[. ]+$/g, '')
      .slice(0, 200);
    return cleaned || 'download';
  }

  const api = { ShareError, keyFromFragment, createDecryptor, createSplitter, safeName };
  if (typeof module !== 'undefined' && module.exports) {
    module.exports = api;
    return;
  }

  // In the page.
  const SEALED_PROBLEMS = {
    key: 'This link is incomplete: the part after the # is missing or was changed. Ask for the link again.',
    format: 'This is not a Fetchpath file, or it needs a newer browser or Fetchpath.',
    damaged: 'The file did not arrive intact, so nothing was saved. Ask the sender for a new link.',
  };
  const message = document.getElementById('message');
  const button = document.getElementById('start');
  const progress = document.getElementById('progress');

  function say(text) {
    message.textContent = text;
  }

  async function fetchFrom(offset, signal) {
    const headers = offset > 0 ? { Range: 'bytes=' + offset + '-' } : {};
    const response = await fetch(location.pathname + '?d=1', {
      headers, signal, cache: 'no-store', credentials: 'omit', referrerPolicy: 'no-referrer',
    });
    if (offset > 0) {
      const range = response.headers.get('Content-Range') || '';
      if (response.status === 410) throw new ShareError('gone');
      if (response.status !== 206 || !range.startsWith('bytes ' + offset + '-')) throw new ShareError('resume');
    } else if (response.status !== 200) {
      throw new ShareError(response.status === 410 ? 'gone' : 'unavailable');
    }
    return response;
  }

  async function download(keyBytes) {
    const decryptor = await createDecryptor(keyBytes);
    const splitter = createSplitter();
    const parts = [];
    let saved = 0;
    const keep = (plain) => {
      for (const piece of plain) {
        const file = splitter.feed(piece);
        if (file.length) {
          parts.push(file);
          saved += file.length;
        }
      }
      progress.textContent = saved ? Math.round(saved / 1024 / 1024 * 10) / 10 + ' MB received' : '';
    };
    let offset = 0;
    for (let attempt = 0; ; attempt += 1) {
      try {
        const response = await fetchFrom(offset);
        const reader = response.body.getReader();
        for (;;) {
          const { done, value } = await reader.read();
          if (done) break;
          keep(await decryptor.push(value));
        }
        keep(await decryptor.finish());
        break;
      } catch (error) {
        if (error instanceof ShareError || attempt >= 4) throw error;
        // The connection dropped: continue from the last whole chunk.
        offset = decryptor.resumeAt();
        say('The connection dropped. Trying again...');
        await new Promise((resolve) => setTimeout(resolve, 1000 * (attempt + 1)));
      }
    }
    if (splitter.name() === null) throw new ShareError('damaged');
    const link = document.createElement('a');
    link.href = URL.createObjectURL(new Blob(parts, { type: 'application/octet-stream' }));
    link.download = safeName(splitter.name());
    document.body.appendChild(link);
    link.click();
    link.remove();
    setTimeout(() => URL.revokeObjectURL(link.href), 60000);
  }

  let key = null;
  try {
    key = keyFromFragment(location.hash.slice(1));
  } catch (error) {
    say(SEALED_PROBLEMS.key);
    button.hidden = true;
  }
  if (key !== null) {
    button.addEventListener('click', async () => {
      button.disabled = true;
      say('Downloading and decrypting in this browser...');
      try {
        await download(key);
        say('Done. The file was saved to your downloads. This link works once and is now used up.');
        button.hidden = true;
      } catch (error) {
        const known = error instanceof ShareError && SEALED_PROBLEMS[error.message];
        say(known || (error instanceof ShareError && error.message === 'gone'
          ? 'This link has already been used, expired or was withdrawn.'
          : 'The download could not finish. Check the connection and open the link again.'));
        button.hidden = true;
      }
    });
    button.disabled = false;
  }
}());
