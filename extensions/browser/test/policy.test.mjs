import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const context = vm.createContext({ URL });
vm.runInContext(readFileSync(new URL("../policy.js", import.meta.url), "utf8"), context);
const policy = context.FetchpathPolicy;

test("accepts only HTTP sources and enforces exact-origin exclusions", () => {
  assert.equal(policy.normalizeOrigin("https://files.example.test/a"), "https://files.example.test");
  assert.equal(policy.normalizeOrigin("blob:https://files.example.test/id"), null);
  assert.equal(policy.isExcluded("https://files.example.test/a", ["https://files.example.test"]), true);
  assert.equal(policy.isExcluded("https://cdn.example.test/a", ["https://files.example.test"]), false);
});

test("redaction and filename derivation never expose URL secrets or path separators", () => {
  const raw = "https://user:pass@files.example.test/folder/report%202026.zip?token=secret#private";
  assert.equal(policy.redactUrl(raw), "https://files.example.test/folder/report%202026.zip");
  assert.equal(policy.suggestedFilename(raw), "report 2026.zip");
  assert.equal(policy.suggestedFilename("https://files.example.test/a%2Fb.txt"), "a_b.txt");
  assert.equal(policy.suggestedFilename("https://files.example.test/con.html"), "_con.html");
  assert.equal(policy.pageFilename("https://files.example.test/con.html", "Title"), "_con.html");
  const longSlug = `https://files.example.test/${"a".repeat(300)}.zip`;
  const clipped = policy.suggestedFilename(longSlug);
  assert.ok(Buffer.byteLength(clipped) <= 240, clipped);
  assert.ok(clipped.endsWith(".zip"), clipped);
  assert.doesNotMatch(policy.suggestedFilename("https://files.example.test/a%E2%80%AEb.txt"), /[\u2028-\u202e\u2066-\u2069]/);
  // Truncation that lands on a reserved stem must still stay <= 240 UTF-8 bytes.
  const reservedOverflow = policy.suggestedFilename(`https://files.example.test/conx.${"a".repeat(236)}`);
  assert.ok(Buffer.byteLength(reservedOverflow) <= 240, reservedOverflow);
  assert.doesNotMatch(reservedOverflow.split(".")[0], /^con$/i);
  // Trailing non-ASCII whitespace before a dot must not leave "." or "..".
  assert.equal(policy.suggestedFilename("https://files.example.test/.%C2%A0."), "download.bin");
  assert.equal(policy.suggestedFilename("https://files.example.test/..%C2%A0."), "download.bin");
});

test("a page send without a file extension is named from the title, always as a safe .html leaf", () => {
  const profile = "https://www.linkedin.com/in/alcinei-oliveira-42781750/?trk=x";
  assert.equal(policy.pageFilename(profile, "Alcinei Oliveira | LinkedIn"), "Alcinei Oliveira LinkedIn.html");
  assert.equal(policy.pageFilename(profile, ""), "www.linkedin.com-alcinei-oliveira-42781750.html");
  assert.equal(policy.pageFilename("https://grok.com/bot/abc", "  "), "grok.com-abc.html");
  assert.equal(policy.pageFilename("https://example.test/", ""), "example.test.html");
  // Names that already carry an extension are left as the URL gives them.
  assert.equal(policy.pageFilename("https://example.test/a/report.pdf", "Title"), "report.pdf");
  // Unsafe titles stay inside the host's rules: 240 bytes, no reserved names, separators or bidi.
  const unsafe = ["CON", "com1.txt", "..", "a/b\\c‮d<>:\"|?*", "x".repeat(500), "\u{1F600}".repeat(500), "\u0000\u0007"];
  for (const title of unsafe) {
    const name = policy.pageFilename(profile, title);
    assert.ok(Buffer.byteLength(name) <= 240, name);
    assert.match(name, /\.html$/);
    assert.doesNotMatch(name, /[<>:"/\\|?*\u0000-\u001f‪-‮⁦-⁩]/);
    assert.doesNotMatch(name.split(".")[0], /^(con|prn|aux|nul|com[1-9]|lpt[1-9])$/i);
  }
});

test("filterCookiesForUrl mirrors bridge RFC 6265 scope and caps at 256 cookies", () => {
  const url = "https://sub.example.test/files/2026/report.zip";
  const kept = policy.filterCookiesForUrl(
    [
      { name: "ok_host", value: "1", domain: "sub.example.test", path: "/files", secure: true, hostOnly: true },
      { name: "ok_parent", value: "2", domain: ".example.test", path: "/", secure: false, hostOnly: false },
      { name: "bad_hostonly", value: "3", domain: "example.test", path: "/", secure: true, hostOnly: true },
      { name: "bad_domain", value: "4", domain: "evil.test", path: "/", secure: true, hostOnly: false },
      { name: "bad_path_prefix", value: "5", domain: "sub.example.test", path: "/file", secure: true, hostOnly: true },
      { name: "bad_ctrl", value: "a\nb", domain: "sub.example.test", path: "/", secure: true, hostOnly: true },
    ],
    url,
  );
  assert.deepEqual(
    Array.from(kept, (c) => c.name),
    ["ok_host", "ok_parent"],
  );

  // Secure cookies are dropped for http:// URLs (e.g. localhost).
  const httpKept = policy.filterCookiesForUrl(
    [
      { name: "sec", value: "1", domain: "localhost", path: "/", secure: true, hostOnly: true },
      { name: "plain", value: "2", domain: "localhost", path: "/", secure: false, hostOnly: true },
    ],
    "http://localhost:8080/a",
  );
  assert.deepEqual(
    Array.from(httpKept, (c) => c.name),
    ["plain"],
  );

  // Over 256 cookies are capped to 256, keeping most-specific paths first.
  const many = Array.from({ length: 270 }, (_, i) => ({
    name: `c${i}`,
    value: "v",
    domain: "sub.example.test",
    path: i === 269 ? "/files/2026" : "/",
    secure: true,
    hostOnly: true,
  }));
  const capped = policy.filterCookiesForUrl(many, url);
  assert.equal(capped.length, 256);
  assert.equal(capped[0].name, "c269");

  // UTF-8 byte length (> 4096 bytes) is enforced even when UTF-16 code units <= 4096.
  const multibyteOver = policy.filterCookiesForUrl(
    [{ name: "wide", value: "€".repeat(2000), domain: "sub.example.test", path: "/", secure: true, hostOnly: true }],
    url,
  );
  assert.equal(multibyteOver.length, 0);
});
