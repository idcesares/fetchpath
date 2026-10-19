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
  const raw = "https://files.example.test/folder/report%202026.zip?token=secret#private";
  assert.equal(policy.redactUrl(raw), "https://files.example.test/folder/report%202026.zip");
  assert.equal(policy.suggestedFilename(raw), "report 2026.zip");
  assert.equal(policy.suggestedFilename("https://files.example.test/a%2Fb.txt"), "a_b.txt");
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
