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
    url.search = "";
    url.hash = "";
    return url.toString();
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
    const safe = decoded.replace(/[<>:"/\\\\|?*\u0000-\u001f]/g, "_").replace(/[. ]+$/g, "");
    return safe || "download.bin";
  }

  // One safe leaf piece: the host's rules (no path or control characters, no
  // bidi overrides, no trailing dot or space) with a character cap that keeps
  // the whole name far under the host's 240-byte limit.
  const UNSAFE_TEXT = new RegExp(
    "[<>:\"/\\\\|?*\u0000-\u001f\u007f-\u009f\u200b-\u200f\u2028-\u202e\u2066-\u2069\ufeff]",
    "g",
  );

  function cleanPiece(text, maxChars) {
    const cleaned = String(text ?? "").replace(UNSAFE_TEXT, " ").replace(/\s+/g, " ").trim();
    return Array.from(cleaned).slice(0, maxChars).join("").replace(/^[. ]+|[. ]+$/g, "");
  }

  const RESERVED_STEM = /^(con|prn|aux|nul|conin\$|conout\$|(com|lpt)[1-9¹²³])$/i;

  // A page send saves HTML, so a name without an extension (a profile or chat
  // URL's last segment) gets one: the tab title, else "<host>-<segment>".
  // A name that already has an extension is kept as the URL gives it.
  function pageFilename(rawUrl, title) {
    const base = suggestedFilename(rawUrl);
    const url = parseHttpUrl(rawUrl);
    const segments = url ? url.pathname.split("/").filter(Boolean) : [];
    if (segments.length > 0 && /[^.]\.[A-Za-z0-9]{1,10}$/.test(base)) return base;
    let stem = cleanPiece(title, 50);
    if (!stem) {
      stem = [cleanPiece(url?.hostname, 60), cleanPiece(base === "download.bin" ? "" : base, 40)]
        .filter(Boolean)
        .join("-");
    }
    if (!stem) stem = "page";
    if (RESERVED_STEM.test(stem.split(".")[0].trimEnd())) stem = `_${stem}`;
    return `${stem}.html`;
  }

  root.FetchpathPolicy = { parseHttpUrl, normalizeOrigin, isExcluded, redactUrl, suggestedFilename, pageFilename };
})(globalThis);
