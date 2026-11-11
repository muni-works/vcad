/**
 * The vcad skill (plugins/vcad/skills/vcad/SKILL.md) ships in the npm tarball,
 * the Mac app bundle, and the Claude Code plugin. This test keeps it from
 * drifting away from the server it describes: every backticked tool name must
 * be a real tool, and the loon example must build.
 */
import { describe, it, expect, beforeAll } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { InMemoryTransport } from "@modelcontextprotocol/sdk/inMemory.js";
import { Engine } from "@vcad/engine";
import { createServer } from "../server.js";

const here = dirname(fileURLToPath(import.meta.url));
const SKILL = readFileSync(
  join(here, "../../../../plugins/vcad/skills/vcad/SKILL.md"),
  "utf8",
);

let call: (name: string, args: Record<string, unknown>) => Promise<{
  content: Array<{ type: string; text?: string }>;
  isError?: boolean;
}>;
let toolNames: Set<string>;

beforeAll(async () => {
  const engine = await Engine.init();
  const server = await createServer(engine, { user: null });
  const client = new Client({ name: "t", version: "1" });
  const [a, b] = InMemoryTransport.createLinkedPair();
  await Promise.all([server.connect(b), client.connect(a)]);
  toolNames = new Set((await client.listTools()).tools.map((t) => t.name));
  call = async (name, args) => (await client.callTool({ name, arguments: args })) as never;
});

describe("vcad skill", () => {
  it("names only tools this server has", () => {
    const named = [...SKILL.matchAll(/`([a-z][a-z0-9]*(?:_[a-z0-9]+)+)`/g)].map((m) => m[1]!);
    expect(named.length).toBeGreaterThan(8);
    const fields = new Set(["document_id", "uptime_s", "instance_id"]);
    const missing = named.filter((n) => !fields.has(n) && !toolNames.has(n));
    expect(missing).toEqual([]);
  });

  it("loon example builds", async () => {
    const src = /```loon\n([\s\S]*?)```/.exec(SKILL)![1]!;
    const r = await call("create_cad_loon", { source: src });
    expect(r.isError, r.content[0]?.text).toBeFalsy();
  });

  it("points at no hosted vcad service", () => {
    expect(SKILL).not.toMatch(/vcad\.io|mcp\.vcad/);
  });
});
