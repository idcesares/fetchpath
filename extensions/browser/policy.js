(function attachPolicy(root) {
  function parseHttpUrl(rawUrl) {
    try {
      const url = new URL(rawUrl);
      return ["http:", "https:"].includes(url.protocol) ? url : null;
    } catch {
      return null;
    }
  }

  function normalizeOrigin(rawUrl) {
    return parseHttpUrl(rawUrl)?.origin ?? null;
  }

  function isExcluded(rawUrl, excludedOrigins) {
    const origin = normalizeOrigin(rawUrl);
    return origin === null || excludedOrigins.includes(origin);
  }

  function redactUrl(rawUrl) {
    const url = parseHttpUrl(rawUrl);
    if (!url) return "<unsupported-url>";
    url.username = "";
    url.password = "";
    url.search = "";
    url.hash = "";
    return url.toString();
  }

  // One safe leaf piece: the host's rules (no path or control characters, no
  // bidi overrides, no trailing dot or space) with a character cap that keeps
  // the whole name far under the host's 240-byte limit.
  const UNSAFE_TEXT = new RegExp(
    "[<>:\"/\\\\|?*\u0000-\u001f\u007f-\u009f\u200b-\u200f\u2028-\u202e\u2066-\u2069\ufeff]",
    "g",
  );

  const RESERVED_STEM = /^(con|prn|aux|nul|conin\$|conout\$|(com|lpt)[1-9¹²³])$/i;
  const MAX_FILENAME_BYTES = 240;

  function utf8CharBytes(ch) {
    const code = ch.codePointAt(0) ?? 0;
    if (code <= 0x7f) return 1;
    if (code <= 0x7ff) return 2;
    if (code <= 0xffff) return 3;
    return 4;
  }

  function utf8ByteLength(text) {
    let total = 0;
    for (const ch of text) total += utf8CharBytes(ch);
    return total;
  }

  function truncateUtf8(text, maxBytes) {
    if (utf8ByteLength(text) <= maxBytes) return text;
    let out = "";
    let used = 0;
    for (const ch of text) {
      const size = utf8CharBytes(ch);
      if (used + size > maxBytes) break;
      out += ch;
      used += size;
    }
    return out;
  }

  function stripTrailingDotsAndSpace(text) {
    return text.replace(/[\s.]+$/u, "").trim();
  }

  function sanitizeFilename(raw, fallback = "download.bin") {
    let safe = stripTrailingDotsAndSpace(String(raw ?? "").replace(UNSAFE_TEXT, "_"));
    if (!safe || safe === "." || safe === "..") return fallback;
    if (RESERVED_STEM.test(safe.split(".")[0].trimEnd())) safe = `_${safe}`;
    if (utf8ByteLength(safe) > MAX_FILENAME_BYTES) {
      const dot = safe.lastIndexOf(".");
      if (dot > 0 && dot < safe.length - 1) {
        const ext = safe.slice(dot);
        const extBytes = utf8ByteLength(ext);
        if (extBytes < MAX_FILENAME_BYTES - 1) {
          const stem = stripTrailingDotsAndSpace(truncateUtf8(safe.slice(0, dot), MAX_FILENAME_BYTES - extBytes));
          safe = stem ? `${stem}${ext}` : truncateUtf8(safe, MAX_FILENAME_BYTES);
        } else {
          safe = truncateUtf8(safe, MAX_FILENAME_BYTES);
        }
      } else {
        safe = truncateUtf8(safe, MAX_FILENAME_BYTES);
      }
      safe = stripTrailingDotsAndSpace(safe);
    }
    if (!safe || safe === "." || safe === "..") return fallback;
    if (RESERVED_STEM.test(safe.split(".")[0].trimEnd())) {
      safe = `_${truncateUtf8(safe, MAX_FILENAME_BYTES - 1)}`;
      safe = stripTrailingDotsAndSpace(safe);
    }
    if (!safe || safe === "." || safe === "..") return fallback;
    return safe;
  }

  function suggestedFilename(rawUrl) {
    const url = parseHttpUrl(rawUrl);
    const candidate = url?.pathname.split("/").filter(Boolean).at(-1) ?? "download.bin";
    let decoded = candidate;
    try {
      decoded = decodeURIComponent(candidate);
    } catch {
      // Keep the encoded segment when it is not valid percent-encoding.
    }
    return sanitizeFilename(decoded, "download.bin");
  }

  function cleanPiece(text, maxChars) {
    const cleaned = String(text ?? "").replace(UNSAFE_TEXT, " ").replace(/\s+/g, " ").trim();
    return Array.from(cleaned).slice(0, maxChars).join("").replace(/^[. ]+|[. ]+$/g, "");
  }

  // A page send saves HTML, so a name without an extension (a profile or chat
  // URL's last segment) gets one: the tab title, else "<host>-<segment>".
  // A name that already has an extension is kept as the URL gives it.
  function pageFilename(rawUrl, title) {
    const base = suggestedFilename(rawUrl);
    const url = parseHttpUrl(rawUrl);
    const segments = url ? url.pathname.split("/").filter(Boolean) : [];
    if (segments.length > 0 && /[^.]\.[A-Za-z0-9]{1,10}$/.test(base)) {
      return sanitizeFilename(base, "page.html");
    }
    let stem = cleanPiece(title, 50);
    if (!stem) {
      stem = [cleanPiece(url?.hostname, 60), cleanPiece(base === "download.bin" ? "" : base, 40)]
        .filter(Boolean)
        .join("-");
    }
    if (!stem) stem = "page";
    if (RESERVED_STEM.test(stem.split(".")[0].trimEnd())) stem = `_${stem}`;
    return sanitizeFilename(`${stem}.html`, "page.html");
  }

  const MAX_COOKIES = 256;

  // Mirrors `crates/fetchpath-browser-inbox/src/lib.rs` cookie_line() scope
  // checks so one mismatched or oversized browser cookie cannot reject a whole
  // download capture.
  function filterCookiesForUrl(rawCookies, rawUrl) {
    const url = rawUrl instanceof URL ? rawUrl : parseHttpUrl(rawUrl);
    if (!url || !Array.isArray(rawCookies)) return [];
    const host = url.hostname.toLowerCase();
    const requestPath = url.pathname || "/";
    const isHttps = url.protocol === "https:";
    const valid = [];
    for (const cookie of rawCookies) {
      if (!cookie?.name || typeof cookie.name !== "string") continue;
      if (typeof cookie.value !== "string" || typeof cookie.domain !== "string" || typeof cookie.path !== "string") continue;
      if (!cookie.path.startsWith("/")) continue;
      if (cookie.secure && !isHttps) continue;
      let badPart = false;
      for (const part of [cookie.name, cookie.value, cookie.domain, cookie.path]) {
        if (utf8ByteLength(part) > 4096 || /[\t\r\n\0]/.test(part)) {
          badPart = true;
          break;
        }
      }
      if (badPart) continue;
      const domain = cookie.domain.replace(/^\.+/, "").toLowerCase();
      if (!domain) continue;
      const hostOnly = Boolean(cookie.hostOnly);
      const domainMatches = host === domain || (!hostOnly && host.endsWith(`.${domain}`));
      const pathMatches =
        requestPath === cookie.path ||
        (requestPath.startsWith(cookie.path) &&
          (cookie.path.endsWith("/") || requestPath[cookie.path.length] === "/"));
      if (!domainMatches || !pathMatches) continue;
      valid.push({ cookie, pathLen: cookie.path.length, hostOnly, domainLen: domain.length });
    }
    if (valid.length > MAX_COOKIES) {
      valid.sort((a, b) => b.pathLen - a.pathLen || Number(b.hostOnly) - Number(a.hostOnly) || b.domainLen - a.domainLen);
      valid.length = MAX_COOKIES;
    }
    return valid.map((entry) => entry.cookie);
  }

  root.FetchpathPolicy = {
    parseHttpUrl,
    normalizeOrigin,
    isExcluded,
    redactUrl,
    sanitizeFilename,
    suggestedFilename,
    pageFilename,
    filterCookiesForUrl,
  };
})(globalThis);
