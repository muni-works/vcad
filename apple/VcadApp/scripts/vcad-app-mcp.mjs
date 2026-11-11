#!/usr/bin/env node
// stdio ↔ HTTP shim for the vcad desktop app's agent bridge.
//
// The app serves MCP on http://127.0.0.1:<port>/mcp and registers itself in
// ~/.vcad/instances/<pid>.json. MCP clients that only speak stdio point here:
//
//   { "command": "node", "args": ["/path/to/vcad/apple/VcadApp/scripts/vcad-app-mcp.mjs"] }
//
// Each newline-delimited JSON-RPC message on stdin is POSTed to the app; the
// JSON response (if any — notifications get 202) is written to stdout.
//
// Which app: VCAD_APP_URL, else VCAD_APP_PID, else the instance whose open
// document is VCAD_APP_DOCUMENT, else the newest (warning on stderr and in the
// next tool result when several run). The choice is pinned for this process:
// another window launching later never steals it. If the pinned app exits we
// re-pick, and the first result from a different instance says so loudly.
import { createInterface } from "node:readline";
import { listInstances, relaunchTracker, selectInstance } from "./vcad-app-discovery.mjs";

let pinned = null; // {url, instance}
let pendingNote = null;
const noticeChange = relaunchTracker();

function endpoint() {
  if (pinned && (process.env.VCAD_APP_URL || listInstances().some((r) => r.pid === pinned.instance?.pid))) {
    return pinned.url;
  }
  const pick = selectInstance({ instances: listInstances() });
  if (pick.error) throw new Error(pick.error);
  if (pick.warning) {
    process.stderr.write(`[vcad-app-mcp] ${pick.warning}\n`);
    pendingNote = pick.warning;
  }
  pinned = pick;
  return pick.url;
}

/** Prepend a notice (relaunch / ambiguity) to a tool result the agent will read. */
function annotate(body) {
  const meta = body?.result?._meta?.["io.vcad/instance"];
  const notes = [noticeChange(meta), pendingNote].filter(Boolean);
  pendingNote = null;
  if (!notes.length) return body;
  if (Array.isArray(body.result?.content)) {
    body.result.content.unshift({ type: "text", text: `⚠ ${notes.join(" ")}` });
  } else {
    process.stderr.write(`[vcad-app-mcp] ${notes.join(" ")}\n`);
  }
  return body;
}

function reply(obj) {
  process.stdout.write(JSON.stringify(obj) + "\n");
}

const rl = createInterface({ input: process.stdin });
rl.on("line", async (line) => {
  if (!line.trim()) return;
  let msg;
  try {
    msg = JSON.parse(line);
  } catch {
    return reply({ jsonrpc: "2.0", id: null, error: { code: -32700, message: "parse error" } });
  }
  try {
    const res = await fetch(endpoint(), {
      method: "POST",
      headers: { "content-type": "application/json", accept: "application/json, text/event-stream" },
      body: line,
    });
    if (res.status === 202 || msg.id === undefined) return;
    reply(annotate(await res.json()));
  } catch (e) {
    if (msg.id === undefined) return;
    reply({
      jsonrpc: "2.0",
      id: msg.id,
      error: { code: -32000, message: `vcad app not reachable (${e.message}). Is vcad.app running? (~/.vcad/instances)` },
    });
  }
});

// Server→client notifications (resource updates for vcad://doc/*) arrive on
// the app's SSE stream; forward each event to stdout. Reconnects when the app
// restarts.
async function listen() {
  for (;;) {
    try {
      const res = await fetch(endpoint(), { headers: { accept: "text/event-stream" } });
      const reader = res.body.getReader();
      const dec = new TextDecoder();
      let buf = "";
      for (;;) {
        const { value, done } = await reader.read();
        if (done) break;
        buf += dec.decode(value, { stream: true });
        let i;
        while ((i = buf.indexOf("\n\n")) >= 0) {
          const ev = buf.slice(0, i);
          buf = buf.slice(i + 2);
          const data = ev.split("\n").filter((l) => l.startsWith("data: ")).map((l) => l.slice(6)).join("\n");
          if (data) process.stdout.write(data + "\n");
        }
      }
    } catch {}
    await new Promise((r) => setTimeout(r, 2000));
  }
}
listen();
rl.on("close", () => process.exit(0));
