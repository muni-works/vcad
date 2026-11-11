#!/usr/bin/env python3
"""End-to-end check of MCP-native Phase B with a real MCP client.

Connects through `vcad mcp` (stdio) to a running vcad.app, exactly as an
agent does, then:

  1. creates a cylinder in the open window (one named undo step, live),
  2. subscribes to vcad://doc/selection and waits for the USER to click a
     face of it in the app (the live resource is how "this face" reaches the
     agent),
  3. fillets that face inside one agent turn ("Agent: rounded the face"),
  4. reads vcad://doc/current back and checks the fillet landed.

Usage:
  scripts/mcp-live-fillet-e2e.py --vcad target/debug/vcad [--pid <vcad.app pid>]
      [--radius 2] [--timeout 180]

`--pid` targets one instance (VCAD_APP_PID); without it `vcad mcp` picks the
newest instance. Exit 0 = every step passed.
"""

import argparse
import json
import os
import queue
import subprocess
import sys
import threading
import time


class Client:
    def __init__(self, cmd, env):
        self.p = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  stderr=sys.stderr, env=env, text=True, bufsize=1)
        self.next_id = 0
        self.responses = {}
        self.notes = queue.Queue()
        self.cv = threading.Condition()
        threading.Thread(target=self._read, daemon=True).start()

    def _read(self):
        for line in self.p.stdout:
            line = line.strip()
            if not line:
                continue
            try:
                msg = json.loads(line)
            except json.JSONDecodeError:
                continue
            if "id" in msg and ("result" in msg or "error" in msg):
                with self.cv:
                    self.responses[msg["id"]] = msg
                    self.cv.notify_all()
            elif "method" in msg:
                self.notes.put(msg)

    def send(self, msg):
        self.p.stdin.write(json.dumps(msg) + "\n")
        self.p.stdin.flush()

    def rpc(self, method, params=None, timeout=120):
        self.next_id += 1
        i = self.next_id
        self.send({"jsonrpc": "2.0", "id": i, "method": method, "params": params or {}})
        deadline = time.time() + timeout
        with self.cv:
            while i not in self.responses:
                left = deadline - time.time()
                if left <= 0:
                    raise TimeoutError(f"{method}: no response in {timeout}s")
                self.cv.wait(left)
            r = self.responses.pop(i)
        if "error" in r:
            raise RuntimeError(f"{method}: {r['error']}")
        return r["result"]

    def tool(self, name, args):
        r = self.rpc("tools/call", {"name": name, "arguments": args})
        text = " ".join(c.get("text", "") for c in r.get("content", []) if c.get("type") == "text")
        if r.get("isError"):
            raise RuntimeError(f"{name} failed: {text}")
        return text

    def resource(self, uri):
        r = self.rpc("resources/read", {"uri": uri})
        return json.loads(r["contents"][0]["text"])


def snap(c, out, name):
    """Save the live viewport (the app's own snapshot tool) as <out>/<name>.png."""
    if not out:
        return
    import base64
    os.makedirs(out, exist_ok=True)
    try:
        # An occluded / off-Space window renders no frames, so the viewport
        # snapshot can't complete; don't let proof-taking hang the check.
        r = c.rpc("tools/call", {"name": "snapshot", "arguments": {}}, timeout=15)
    except TimeoutError:
        step(f"snapshot {name}: viewport not rendering (window hidden?) — skipped")
        return
    for block in r.get("content", []):
        if block.get("type") == "image":
            path = os.path.join(out, name + ".png")
            with open(path, "wb") as f:
                f.write(base64.b64decode(block["data"]))
            step(f"snapshot → {path}")
            return
    step(f"snapshot unavailable: {r}")


def hook(a, name):
    """Run --on-step CMD NAME (e.g. a window capture) at each proof point."""
    if a.on_step:
        subprocess.run(a.on_step + [name], check=False)
    time.sleep(a.pause)


def step(msg):
    print(f"[e2e] {msg}", flush=True)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--vcad", default="vcad", help="path to the vcad CLI")
    ap.add_argument("--pid", help="vcad.app instance to target")
    ap.add_argument("--radius", type=float, default=2.0)
    ap.add_argument("--timeout", type=float, default=180, help="seconds to wait for the face click")
    ap.add_argument("--select", choices=["click", "tool"], default="click",
                    help="click: wait for a person to click the top face; tool: select it with "
                         "select_edges {face:'top'} (unattended runs)")
    ap.add_argument("--on-step", nargs="+", help="command run with the step name at each proof point")
    ap.add_argument("--pause", type=float, default=0, help="seconds to hold at each proof point")
    ap.add_argument("--save-doc", help="write the final live document (IR JSON) here")
    ap.add_argument("--out", help="directory for viewport snapshots (before/selected/after PNGs)")
    a = ap.parse_args()

    env = dict(os.environ)
    if a.pid:
        env["VCAD_APP_PID"] = a.pid
    c = Client([a.vcad, "mcp"], env)

    init = c.rpc("initialize", {"protocolVersion": "2025-06-18",
                                "clientInfo": {"name": "claude-code", "version": "e2e"},
                                "capabilities": {}})
    c.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
    step(f"connected to {init['serverInfo']}")

    info = c.tool("server_info", {})
    assert "app" in info.lower() or "vcad" in info.lower(), info
    step("server_info: " + info.splitlines()[0][:120])

    text = c.tool("create", {"type": "cylinder", "name": "E2E Post",
                             "params": {"radius": 15, "height": 30}})
    step(f"create → {text[:100]}")

    c.rpc("resources/subscribe", {"uri": "vcad://doc/selection"})
    if a.select == "tool":
        step("selecting the top face via select_edges (unattended)")
        part = c.resource("vcad://doc/view")["visible_parts"][-1]["index"]
        c.tool("select_edges", {"part": part, "face": "top"})
    else:
        step("subscribed to vcad://doc/selection — click the TOP FACE of 'E2E Post' in vcad.app now")

    deadline = time.time() + a.timeout
    face = None
    notified = False
    while time.time() < deadline and face is None:
        # Drain every queued notification (view/doc updates arrive too).
        try:
            n = c.notes.get(timeout=1.0)
            while True:
                if n.get("method") == "notifications/resources/updated" and \
                        n.get("params", {}).get("uri") == "vcad://doc/selection":
                    notified = True
                n = c.notes.get_nowait()
        except queue.Empty:
            pass
        sel = c.resource("vcad://doc/selection")
        faces = [t for t in sel.get("topology", []) if t.get("kind") == "face"]
        if faces:
            face = faces[0]
    if face is None:
        step("FAIL: no face selected in time")
        return 1
    step(f"user picked face {face['id']} on part {face['part']} at {face.get('center')} "
         f"(update notification: {'yes' if notified else 'no, read on poll'})")

    snap(c, a.out, "1-selected")
    hook(a, "1-selected")
    doc_id = c.resource("vcad://doc/view").get("document_id")
    c.tool("agent_turn", {"label": "rounded the face"})
    text = c.tool("fillet_edges", {"radius": a.radius, "document_id": doc_id})
    c.tool("agent_turn", {"end": True})
    step(f"fillet_edges → {text}")

    snap(c, a.out, "2-filleted")
    hook(a, "2-filleted")
    doc = c.resource("vcad://doc/current")
    blob = json.dumps(doc.get("document"))
    ok = "EdgeBlend" in blob or "Fillet" in blob or "edge_blend" in blob
    if a.save_doc:
        with open(a.save_doc, "w") as f:
            json.dump(doc.get("document"), f, indent=1)
    step(f"document after: revision {doc.get('revision')}, fillet present: {ok}")
    if "Agent: rounded the face" not in text:
        step("FAIL: undo label is not the turn's")
        return 1
    if not ok:
        step("FAIL: no fillet in the document")
        return 1
    step("PASS")
    c.p.stdin.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
