import { describe, it, expect, beforeEach } from "vitest";
import { gzipSync } from "node:zlib";
import { continueDocument } from "../tools/continue-doc.js";
import { documents } from "../tools/session.js";

/** gzip + base64url, mirroring the inline-doc handoff encoder. */
function encodeInline(obj: unknown): string {
  return gzipSync(Buffer.from(JSON.stringify(obj), "utf-8"))
    .toString("base64")
    .replace(/\+/g, "-")
    .replace(/\//g, "_")
    .replace(/=+$/, "");
}

const UUID = "11111111-2222-3333-4444-555555555555";
const json = (r: { content: Array<{ text: string }> }) =>
  JSON.parse(r.content[0].text);

beforeEach(() => {
  documents.clear();
});

describe("continueDocument (local build)", () => {
  it("errors when neither token nor doc is given", async () => {
    const r = await continueDocument({});
    expect(r.isError).toBe(true);
    expect(r.content[0].text).toMatch(/inline `doc`/);
  });

  it("fails closed on a share token — the hosted resolver is gone", async () => {
    process.env.SUPABASE_URL = "https://supa.test";
    process.env.SUPABASE_SERVICE_ROLE_KEY = "svc";
    try {
      const r = await continueDocument({ token: UUID });
      expect(r.isError).toBe(true);
      expect(r.content[0].text).toMatch(/not available in the local build/);
      expect(documents.size).toBe(0);
    } finally {
      delete process.env.SUPABASE_URL;
      delete process.env.SUPABASE_SERVICE_ROLE_KEY;
    }
  });

  it("opens a session from an inline `doc` handoff", async () => {
    const blob = encodeInline({ version: 1, nodes: {}, roots: ["a", "b"] });
    const r = await continueDocument({ doc: blob });
    expect(r.isError).toBeFalsy();
    const out = json(r);
    expect(out.parts).toBe(2);
    expect(documents.has(out.document_id)).toBe(true);
  });

  it("errors with a fresh-handoff hint on a corrupt inline `doc`", async () => {
    const r = await continueDocument({ doc: "not-valid-gzip-base64" });
    expect(r.isError).toBe(true);
    expect(r.content[0].text).toMatch(/fresh handoff/);
  });
});
