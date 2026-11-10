/**
 * open_in_app emits a `vcad://` deep link into the native app (format:
 * docs/launch/vcad-url-scheme.md), with the download page as the fallback.
 */

import { afterEach, beforeAll, describe, expect, it } from "vitest";
import { gunzipSync } from "node:zlib";
import { Engine } from "@vcad/engine";
import type { Document } from "@vcad/ir";
import { registerSession } from "../tools/session-core.js";
import {
  VCAD_DOWNLOAD_URL,
  buildAppLink,
  openInApp,
  toolDefs,
} from "../tools/share.js";

let engine: Engine;
let cube: Document;

beforeAll(async () => {
  engine = await Engine.init();
  const doc = engine.evalVcadSource("[cube 5 5 5]");
  if (!doc) throw new Error("engine build lacks loon support");
  cube = doc;
});

const env = { ...process.env };
afterEach(() => {
  process.env = { ...env };
});

function decodeDoc(url: string): unknown {
  const doc = new URL(url).searchParams.get("doc")!;
  const b64 = doc.replace(/-/g, "+").replace(/_/g, "/");
  return JSON.parse(gunzipSync(Buffer.from(b64, "base64")).toString("utf8"));
}

describe("buildAppLink", () => {
  it("sends a local disk-store session by id", () => {
    delete process.env.VCAD_MCP_SESSION_DIR;
    const link = buildAppLink(cube, { documentId: "doc_abc-123", name: "cube", fileStore: true });
    expect(link.via).toBe("session");
    const u = new URL(link.url);
    expect(u.protocol).toBe("vcad:");
    expect(u.host).toBe("open");
    expect(u.searchParams.get("session")).toBe("doc_abc-123");
    expect(u.searchParams.get("dir")).toBeNull();
    expect(u.searchParams.get("name")).toBe("cube");
  });

  it("names a non-default session dir", () => {
    process.env.VCAD_MCP_SESSION_DIR = "/tmp/vcad-sessions-x";
    const link = buildAppLink(cube, { documentId: "doc_1", fileStore: true });
    expect(new URL(link.url).searchParams.get("dir")).toBe("/tmp/vcad-sessions-x");
  });

  it("embeds the document as gzip+base64url JSON without a disk store", () => {
    const link = buildAppLink(cube, { documentId: "doc_1", fileStore: false });
    expect(link.via).toBe("inline");
    expect(decodeDoc(link.url)).toEqual(JSON.parse(JSON.stringify(cube)));
  });

  it("never puts a path-like id in a session link", () => {
    const link = buildAppLink(cube, { documentId: "../../etc/passwd", fileStore: true });
    expect(link.via).toBe("inline");
  });
});

describe("open_in_app tool", () => {
  it("returns a vcad:// link and the download fallback", () => {
    const id = registerSession(cube);
    const text = openInApp({ document_id: id, name: "cube" }).content[0].text;
    expect(text).toMatch(/vcad:\/\/open\?/);
    expect(text).toContain(VCAD_DOWNLOAD_URL);
    expect(text).not.toContain("vcad.io/#/new");
  });

  it("accepts an inline document", () => {
    const text = openInApp({ document: JSON.stringify(cube) }).content[0].text;
    const url = text.match(/vcad:\/\/open\?\S+/)![0];
    expect(decodeDoc(url)).toEqual(JSON.parse(JSON.stringify(cube)));
  });

  it("never emits a vcad.io web link and drops open_in_browser", () => {
    process.env.VCAD_LEGACY_WEB_LINKS = "1";
    try {
      const text = openInApp({ document: JSON.stringify(cube) }).content[0].text;
      expect(text).not.toContain("/#/new?");
    } finally {
      delete process.env.VCAD_LEGACY_WEB_LINKS;
    }
    expect(toolDefs.map((t) => t.name)).toEqual(["open_in_app"]);
  });
});
