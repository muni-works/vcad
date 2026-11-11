import { describe, it, expect, afterEach, beforeEach } from "vitest";
import { Readable } from "node:stream";
import { createHash } from "node:crypto";
import type { IncomingMessage, ServerResponse } from "node:http";
import {
  storeArtifact,
  getArtifact,
  getArtifactFile,
  resolveArtifact,
  resolveArtifactRef,
  parseArtifactId,
  buildManifest,
  bundleBytes,
  clearArtifacts,
} from "../tools/artifact-store.js";
import { handleArtifactRequest } from "../artifact-route.js";
import { importStep } from "../tools/import.js";
import { documents } from "../tools/session.js";
import type { Engine } from "@vcad/engine";

const sha = (s: string) => createHash("sha256").update(Buffer.from(s, "utf8")).digest("hex");

afterEach(() => {
  clearArtifacts();
  delete process.env.MCP_MAX_INLINE_ARTIFACT_BYTES;
  delete process.env.VCAD_MCP_PUBLIC_URL;
  documents.clear();
});

describe("artifact-store", () => {
  it("hashes a manifest and counts bundle bytes without storing", () => {
    const files = [
      { name: "a.gbr", content: "AAAA" },
      { name: "b.drl", content: "BB" },
    ];
    expect(bundleBytes(files)).toBe(6);
    const manifest = buildManifest(files);
    expect(manifest).toEqual([
      { file: "a.gbr", bytes: 4, sha256: sha("AAAA") },
      { file: "b.drl", bytes: 2, sha256: sha("BB") },
    ]);
  });

  it("stores a bundle and round-trips file bytes by id", () => {
    const handle = storeArtifact([
      { name: "top.gbr", content: "G04 top*" },
      { name: "out.drl", content: "M48" },
    ]);
    expect(handle.artifact_id).toMatch(/^art_/);
    expect(handle.artifact_url).toMatch(/\/artifacts\/art_/);
    expect(handle.bytes).toBe("G04 top*".length + "M48".length);
    expect(handle.manifest).toHaveLength(2);

    const stored = getArtifact(handle.artifact_id);
    expect(stored).not.toBeNull();
    expect(stored!.files).toHaveLength(2);

    const file = getArtifactFile(handle.artifact_id, "top.gbr");
    expect(file).not.toBeNull();
    expect(file!.buf.toString("utf8")).toBe("G04 top*");
  });

  it("mints relative URLs unless VCAD_MCP_PUBLIC_URL names an origin", () => {
    process.env.SUPABASE_URL = "https://fake.supabase.co";
    process.env.SUPABASE_SERVICE_ROLE_KEY = "service-role-test-key";
    try {
      const local = storeArtifact([{ name: "x.gbr", content: "hi" }]);
      expect(local.artifact_url).toBe(`/artifacts/${local.artifact_id}`);
      expect(parseArtifactId(local.artifact_url)).toBe(local.artifact_id);
    } finally {
      delete process.env.SUPABASE_URL;
      delete process.env.SUPABASE_SERVICE_ROLE_KEY;
    }
  });

  it("resolves a handle by raw id and by artifact_url", () => {
    process.env.VCAD_MCP_PUBLIC_URL = "https://mcp.example.com";
    const handle = storeArtifact([{ name: "x.gbr", content: "hi" }]);

    expect(parseArtifactId(handle.artifact_id)).toBe(handle.artifact_id);
    expect(parseArtifactId(handle.artifact_url)).toBe(handle.artifact_id);

    expect(resolveArtifact(handle.artifact_id)?.id).toBe(handle.artifact_id);
    expect(resolveArtifact(handle.artifact_url)?.id).toBe(handle.artifact_id);

    const ref = resolveArtifactRef(handle.artifact_url);
    expect(ref?.artifact_id).toBe(handle.artifact_id);
    expect(ref?.bytes).toBe(2);
    expect(ref?.manifest).toHaveLength(1);

    expect(resolveArtifact("art_nope")).toBeNull();
    expect(resolveArtifactRef("art_nope")).toBeNull();
  });
});

// ── HTTP route ────────────────────────────────────────────────────────────

function makeReq(method: string, path: string): IncomingMessage {
  const req = Readable.from([]);
  Object.assign(req, { method, url: path, headers: { host: "mcp.vcad.io" } });
  return req as unknown as IncomingMessage;
}
interface CapRes {
  statusCode: number;
  headers: Record<string, string>;
  body: string;
  bodyBuf: Buffer | null;
  writeHead(s: number, h?: Record<string, string>): CapRes;
  end(b?: string | Buffer): void;
}
function makeRes(): CapRes {
  return {
    statusCode: 0,
    headers: {},
    body: "",
    bodyBuf: null,
    writeHead(s, h) {
      this.statusCode = s;
      if (h) this.headers = h;
      return this;
    },
    end(b) {
      if (Buffer.isBuffer(b)) {
        this.bodyBuf = b;
        this.body = b.toString("utf8");
      } else {
        this.body = b ?? "";
      }
    },
  };
}
const res = () => makeRes() as unknown as ServerResponse & CapRes;

describe("handleArtifactRequest", () => {
  it("ignores non-/artifacts paths", async () => {
    const r = res();
    expect(await handleArtifactRequest(makeReq("GET", "/mcp"), r)).toBe(false);
  });

  it("serves the manifest index and each file's raw bytes", async () => {
    const handle = storeArtifact([
      { name: "top.gbr", content: "G04 top*" },
      { name: "out.drl", content: "M48" },
    ]);

    const idx = res();
    expect(await handleArtifactRequest(makeReq("GET", `/artifacts/${handle.artifact_id}`), idx)).toBe(true);
    expect(idx.statusCode).toBe(200);
    const manifest = JSON.parse(idx.body);
    expect(manifest.artifact_id).toBe(handle.artifact_id);
    expect(manifest.files).toHaveLength(2);
    expect(manifest.files[0].url).toMatch(/\/artifacts\/art_.+\/top\.gbr$/);

    const file = res();
    expect(
      await handleArtifactRequest(makeReq("GET", `/artifacts/${handle.artifact_id}/top.gbr`), file),
    ).toBe(true);
    expect(file.statusCode).toBe(200);
    expect(file.headers["Content-Type"]).toBe("application/vnd.gerber");
    expect(file.bodyBuf?.toString("utf8")).toBe("G04 top*");
  });

  it("404s an unknown artifact id", async () => {
    const r = res();
    expect(await handleArtifactRequest(makeReq("GET", "/artifacts/art_missing"), r)).toBe(true);
    expect(r.statusCode).toBe(404);
  });
});

// ── import_step offload ─────────────────────────────────────────────────────

describe("import_step large-result offload", () => {
  // Stub of the kernel's mesh-document entry point: one ImportedMesh root
  // with `n` positions / indices, plus the given skipped-face report.
  const meshDocEngine = (n: number, report: unknown[] = [], summary: string | null = null) =>
    ({
      importStepMeshDocument: (_: ArrayBuffer, partName: string, material: string, source: string) => ({
        document: {
          version: "0.1",
          nodes: {
            "1": {
              id: 1,
              name: partName,
              op: {
                type: "ImportedMesh",
                positions: Array.from({ length: n }, (_, i) => i % 9),
                indices: Array.from({ length: n }, (_, i) => i % 3),
                source,
              },
            },
          },
          materials: {},
          part_materials: { [partName]: material },
          roots: [{ root: 1, material }],
        },
        report,
        summary,
        bodies: 1,
        triangles: n / 3,
        vertices: n / 3,
      }),
    }) as unknown as Engine;
  const smallEngine = meshDocEngine(9);
  const bigEngine = meshDocEngine(40_000);

  const step64 = Buffer.from("x").toString("base64");

  // These cases are about report surfacing and large-result offload, not about
  // which representation import_step defaults to — the stub engines implement
  // only the mesh entry point, so they ask for `as_mesh` explicitly. B-rep
  // import is covered in step-import.test.ts against the real kernel.


  it("surfaces skipped faces from the import report", () => {
    const dirtyEngine = meshDocEngine(
      9,
      [
        {
          solid_id: 45,
          total_faces: 3,
          skipped_faces: [
            { face_id: 29, surface_id: 40, reason: "DEGENERATE_TOROIDAL_SURFACE" },
          ],
          notes: [],
        },
      ],
      "solid #45: skipped 1 of 3 faces",
    );
    const res = importStep({ content_base64: step64, name: "holey", as_mesh: true }, dirtyEngine);
    const out = JSON.parse(res.content[0].text);
    expect(out.summary.warning).toContain("1 face(s) skipped");
    expect(out.summary.skipped_faces).toEqual([
      { solid_id: 45, face_id: 29, surface_id: 40, reason: "DEGENERATE_TOROIDAL_SURFACE" },
    ]);
  });

  it("keeps a small import inline", () => {
    const res = importStep({ content_base64: step64, name: "tiny", as_mesh: true }, smallEngine);
    const out = JSON.parse(res.content[0].text);
    expect(out.document).toBeTruthy();
    expect(out.document.version).toBeTruthy();
    expect(out.artifact_url).toBeUndefined();
  });

  it("offloads a large import to a session + artifact handle (no inline IR)", () => {
    const res = importStep({ content_base64: step64, name: "huge", as_mesh: true }, bigEngine);
    const out = JSON.parse(res.content[0].text);
    // The multi-MB IR never enters context.
    expect(out.document).toBeUndefined();
    expect(out.document_id).toBeTruthy();
    expect(documents.has(out.document_id)).toBe(true);
    expect(out.artifact_url).toMatch(/\/artifacts\/art_/);
    expect(out.manifest).toHaveLength(1);
    expect(out.manifest[0].file).toBe("huge.vcad");
    expect(out.summary.bodies).toBe(1);
  });
});

// ── Local-only store ────────────────────────────────────────────────────────

import { artifactStoreInfo, isArtifactStoreDurable } from "../tools/artifact-store.js";

describe("artifact store is local-only", () => {
  it("reports in-memory even with Supabase env set (no hosted backend)", () => {
    process.env.SUPABASE_URL = "https://fake.supabase.co";
    process.env.SUPABASE_SERVICE_ROLE_KEY = "service-role-test-key";
    try {
      expect(isArtifactStoreDurable()).toBe(false);
      expect(artifactStoreInfo()).toEqual({ artifact_store: "in-memory" });
    } finally {
      delete process.env.SUPABASE_URL;
      delete process.env.SUPABASE_SERVICE_ROLE_KEY;
    }
  });
});
