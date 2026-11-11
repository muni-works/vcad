#!/usr/bin/env python3
"""MCP Phase C live check: a real MCP client (2026 protocol, Tasks) against a
running vcad.app.

  scripts/mcp-phase-c-demo.py [--url http://127.0.0.1:47361/mcp] <step>

Steps (each prints what the agent sees):
  scene      two blocks 6 mm apart + a 2 mm clearance receipt (Holds)
  pick       "which edge?" — blocks until the user clicks an edge in the viewport
  fea        a long FEA study as an MCP Task; polls until it ends (cancel it
             from the app's header pill ✕ and the Task reports cancelled)
  fea-cancel same, but the agent cancels the Task after the first progress
  violate    nudge block B into block A, then verify_receipts (Violated)
"""

import argparse
import json
import os
import tempfile
import sys
import time
import urllib.request

META = {
    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
    "io.modelcontextprotocol/clientInfo": {"name": "phase-c-demo", "version": "1"},
    "io.modelcontextprotocol/clientCapabilities": {
        "extensions": {"io.modelcontextprotocol/tasks": {}},
        "elicitation": {},
    },
}


STATE = os.path.join(tempfile.gettempdir(), "vcad-phase-c-demo.json")


class Client:
    def __init__(self, url):
        self.url = url
        self.n = 0

    def rpc(self, method, params=None, timeout=900):
        self.n += 1
        params = dict(params or {})
        params["_meta"] = META
        body = json.dumps({"jsonrpc": "2.0", "id": self.n, "method": method, "params": params}).encode()
        headers = {"Content-Type": "application/json", "Accept": "application/json, text/event-stream",
                   "Mcp-Method": method}
        if method == "tools/call":
            headers["Mcp-Name"] = params["name"]
        req = urllib.request.Request(self.url, data=body, headers=headers)
        with urllib.request.urlopen(req, timeout=timeout) as r:
            text = r.read().decode()
        if text.startswith("event:") or text.startswith("data:"):
            text = [l[5:] for l in text.splitlines() if l.startswith("data:")][-1]
        msg = json.loads(text)
        if "error" in msg:
            raise SystemExit(f"{method}: {msg['error']}")
        return msg["result"]

    def call(self, name, args=None, **kw):
        return self.rpc("tools/call", {"name": name, "arguments": args or {}}, **kw)


def show(label, result):
    sc = result.get("structuredContent")
    text = next((c.get("text") for c in result.get("content", []) if c.get("type") == "text"), "")
    print(f"── {label}  isError={result.get('isError', False)}")
    print(json.dumps(sc, indent=2)[:1500] if sc is not None else text[:1500])


def fea(c, agent_cancels):
    args = {"part_index": 0, "material": "steel", "resolution": 24, "levels": 4,
            "supports": [{"region": {"min": [-1, -1, -1], "max": [1, 21, 21]}}],
            "loads": [{"region": {"min": [39, -1, -1], "max": [41, 21, 21]}, "force": [0, 0, -500]}]}
    t = c.call("analyze_structure", args)
    assert t.get("resultType") == "task", t
    tid = t["taskId"]
    print(f"task {tid}: {t['status']}")
    last = None
    while True:
        g = c.rpc("tasks/get", {"taskId": tid})
        line = f"{g['status']}: {g.get('statusMessage', '')}"
        if line != last:
            print(" ", line)
            last = line
        if agent_cancels and g["status"] == "working" and "level" in g.get("statusMessage", ""):
            print("  agent → tasks/cancel")
            c.rpc("tasks/cancel", {"taskId": tid})
            agent_cancels = False
        if g["status"] in ("completed", "failed", "cancelled"):
            if g["status"] == "completed":
                show("FEA result", g["result"])
            return
        time.sleep(0.3)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", default="http://127.0.0.1:47361/mcp")
    ap.add_argument("step", choices=["scene", "pick", "fea", "fea-cancel", "violate"])
    a = ap.parse_args()
    c = Client(a.url)
    if a.step == "scene":
        show("A", c.call("create", {"type": "cube", "name": "A", "params": {"size": {"x": 40, "y": 20, "z": 20}}}))
        b = c.call("create", {"type": "cube", "name": "B", "params": {"size": {"x": 20, "y": 20, "z": 20}}})
        show("B", b)
        bid = b["structuredContent"]["created_part_id"]
        json.dump({"B": bid}, open(STATE, "w"))
        show("move B", c.call("translate", {"child": bid, "offset": {"x": 46, "y": 0, "z": 0}}))
        show("receipt", c.call("assert_clearance", {"label": "A–B gap", "group_a": ["A"], "group_b": ["B"], "min_mm": 2}))
    elif a.step == "pick":
        r = c.call("pick_in_viewport", {"message": "Which edge should get the fillet?", "kind": "edge"})
        show("picked", r)
    elif a.step in ("fea", "fea-cancel"):
        fea(c, a.step == "fea-cancel")
    elif a.step == "violate":
        bid = json.load(open(STATE))["B"]
        show("nudge B", c.call("translate", {"child": bid, "offset": {"x": -5, "y": 0, "z": 0}}))
        show("verify_receipts", c.call("verify_receipts"))


if __name__ == "__main__":
    sys.exit(main())
