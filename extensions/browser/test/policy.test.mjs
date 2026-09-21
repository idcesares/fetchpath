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
