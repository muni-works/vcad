import { describe, expect, it } from "vitest";
import { unzipSync, strFromU8 } from "fflate";
import {
  buildExportZip,
  cloudContentToFile,
  exportZipName,
  fetchAllDocuments,
  safeFileName,
  type ExportableDocument,
} from "@/lib/export-all";

const ir = { version: "0.1", nodes: {}, roots: [] };
const crdt = { replica_id: "r1", ops: [] };

function doc(id: string, name: string, content: unknown): ExportableDocument {
  return { id, name, content, updated_at: "2026-11-01T00:00:00Z" };
}

describe("export-all", () => {
  it("pages until a short page", async () => {
    const rows = Array.from({ length: 7 }, (_, i) => doc(`d${i}`, `n${i}`, ir));
    const calls: Array<[number, number]> = [];
    const progress: number[] = [];
    const all = await fetchAllDocuments(
      async (offset, limit) => {
        calls.push([offset, limit]);
        return rows.slice(offset, offset + limit);
      },
      (n) => progress.push(n),
      3,
    );
    expect(all.map((d) => d.id)).toEqual(rows.map((d) => d.id));
    expect(calls).toEqual([[0, 3], [3, 3], [6, 3]]);
    expect(progress).toEqual([3, 6, 7]);
  });

  it("writes the on-disk payload for each content shape", () => {
    expect(cloudContentToFile(crdt)).toEqual({ ext: "vcad", text: JSON.stringify(crdt) });
    expect(cloudContentToFile(ir)).toEqual({ ext: "vcad", text: JSON.stringify(ir) });
    expect(cloudContentToFile({ loonSource: "[cube 1 1 1]" })).toEqual({ ext: "loon", text: "[cube 1 1 1]" });
  });

  it("zips every document with deduped safe names and a manifest", () => {
    const zip = buildExportZip(
      [
        doc("a", "Bracket", ir),
        doc("b", "bracket", crdt),
        doc("c", "a/b:c", { loonSource: "[cube 1 1 1]" }),
        doc("d", "  ", ir),
      ],
      new Date("2026-11-06T12:00:00Z"),
    );
    const files = unzipSync(zip);
    expect(Object.keys(files).sort()).toEqual(
      ["Bracket.vcad", "bracket (2).vcad", "a_b_c.loon", "Untitled.vcad", "manifest.json"].sort(),
    );
    expect(JSON.parse(strFromU8(files["bracket (2).vcad"]!))).toEqual(crdt);
    expect(strFromU8(files["a_b_c.loon"]!)).toBe("[cube 1 1 1]");
    const manifest = JSON.parse(strFromU8(files["manifest.json"]!));
    expect(manifest.count).toBe(4);
    expect(manifest.exported_at).toBe("2026-11-06T12:00:00.000Z");
    expect(manifest.documents[1]).toMatchObject({ file: "bracket (2).vcad", id: "b" });
  });

  it("names things predictably", () => {
    expect(safeFileName("..hidden")).toBe("hidden");
    expect(exportZipName(new Date("2026-11-06T12:00:00Z"))).toBe("vcad-export-2026-11-06.zip");
  });
});
