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
    const safe = decoded.replace(/[<>:"/\\|?*\u0000-\u001f]/g, "_").replace(/[. ]+$/g, "");
    return safe || "download.bin";
  }

  root.FetchpathPolicy = { parseHttpUrl, normalizeOrigin, isExcluded, redactUrl, suggestedFilename };
})(globalThis);
