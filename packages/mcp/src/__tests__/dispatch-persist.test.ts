import {
  describe,
  it,
  expect,
  beforeAll,
  beforeEach,
  afterEach,
} from "vitest";
import { Engine } from "@vcad/engine";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { InMemoryTransport } from "@modelcontextprotocol/sdk/inMemory.js";
import { createServer } from "../server.js";
import { existsSync, mkdtempSync, readdirSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { documents } from "../tools/session.js";

/**
 * End-to-end coverage of the dispatch persist/hydrate wrapper: drives the real
 * CallTool handler in createServer through an in-memory MCP client, with a temp
 * disk session store, so the per-request session scope + persist gating are
 * exercised end to end. (The persist wrapper was previously only
 * typecheck-verified.)
 */

/** Point the disk session store at a fresh temp dir. `has(id)` says whether
 *  that session reached disk. */
function installFake() {
  const dir = mkdtempSync(join(tmpdir(), "vcad-mcp-test-"));
  process.env.VCAD_MCP_SESSION_DIR = dir;
  const file = (id: string) => join(dir, `${id}.json`);
  return {
    dir,
    has: (id: string) => existsSync(file(id)),
    mtime: (id: string) => (existsSync(file(id)) ? statSync(file(id)).mtimeMs : -1),
    count: () => readdirSync(dir).filter((f) => f.endsWith(".json")).length,
  };
}

async function connect(
  engine: Engine,
  user: { sub: string; email: string } | null,
) {
  const server = await createServer(engine, { user });
  const [clientT, serverT] = InMemoryTransport.createLinkedPair();
  const client = new Client(
    { name: "test", version: "0.0.0" },
    { capabilities: {} },
  );
  await Promise.all([client.connect(clientT), server.connect(serverT)]);
  return { client, server };
}

function firstText(result: unknown): string {
  const content = (result as { content: Array<{ type: string; text: string }> })
    .content;
  return content[0].text;
}

describe("dispatch persist wrapper (end-to-end through createServer)", () => {
  let engine: Engine;

  beforeAll(async () => {
    engine = await Engine.init();
  });

  beforeEach(() => {
    documents.clear();
  });

  afterEach(() => {
    delete process.env.VCAD_MCP_SESSION_DIR;
  });

  it("persists a creator, hydrates a reader from the store, and never persists a reader", async () => {
    const fake = installFake();
    const { client, server } = await connect(engine, {
      sub: "user-1",
      email: "u@x.test",
    });

    // Creator: open_document → persisted to the store on the way out.
    const open = await client.callTool({ name: "open_document", arguments: {} });
    const documentId = JSON.parse(firstText(open)).document_id as string;
    expect(fake.count()).toBe(1);
    expect(fake.has(documentId)).toBe(true);
    const mtimeAfterOpen = fake.mtime(documentId);
    // Reader runs on a cold cache: only the disk store can satisfy it.
    documents.clear();

    // Reader: get_document runs in a fresh per-request scope, so it can only
    // succeed by hydrating from the durable store — and it must NOT persist.
    const got = await client.callTool({
      name: "get_document",
      arguments: { document_id: documentId },
    });
    expect(JSON.parse(firstText(got)).version).toBeDefined(); // IR round-tripped
    expect(fake.mtime(documentId)).toBe(mtimeAfterOpen); // reader did not write

    await client.close();
    await server.close();
  });

  it("persists a switch-path writer whose id is only in result text (create_schematic)", async () => {
    const fake = installFake();
    const { client, server } = await connect(engine, {
      sub: "user-2",
      email: "v@x.test",
    });

    // create_schematic is NOT a uiTool, so it never gets structuredContent —
    // the persist site must fall back to scanning the result text for the id.
    const res = await client.callTool({
      name: "create_schematic",
      arguments: {},
    });
    expect(res.isError ?? false).toBe(false);
    const documentId = JSON.parse(firstText(res)).document_id as string;
    expect(fake.count()).toBe(1);
    expect(fake.has(documentId)).toBe(true);

    await client.close();
    await server.close();
  });

  it("undo rewinds the last mutation through the real dispatch path", async () => {
    installFake();
    const { client, server } = await connect(engine, {
      sub: "user-3",
      email: "w@x.test",
    });

    // Build a board: create → place → pour. Each call runs in its own
    // per-request scope (signed-in user), so the only way undo can work across
    // calls is the process-global snapshot stack the dispatch layer maintains.
    const created = await client.callTool({ name: "create_schematic", arguments: {} });
    const id = JSON.parse(firstText(created)).document_id as string;
    await client.callTool({
      name: "place_components",
      arguments: { document_id: id, board_width: 40, board_height: 20 },
    });
    const z = await client.callTool({
      name: "add_zone",
      arguments: { document_id: id, net: "GND", fill_board: true },
    });
    expect(JSON.parse(firstText(z)).zones_total).toBe(1);

    // Undo the pour — the snapshot taken *before* add_zone is restored.
    const u = await client.callTool({ name: "undo", arguments: { document_id: id } });
    const undone = JSON.parse(firstText(u));
    expect(undone.success).toBe(true);
    expect(undone.changed).toEqual([
      expect.objectContaining({ action: "removed", kind: "zone", net: "GND" }),
    ]);

    // The persisted/live session no longer has the pour.
    const got = await client.callTool({
      name: "get_document",
      arguments: { document_id: id },
    });
    const doc = JSON.parse(firstText(got)) as {
      nodes: Record<string, { op: { type: string; board?: { zones: unknown[] } } }>;
    };
    const pcbNode = Object.values(doc.nodes).find((n) => n.op.type === "PcbBoard");
    expect(pcbNode?.op.board?.zones).toHaveLength(0);

    await client.close();
    await server.close();
  });
});
