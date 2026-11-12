#!/usr/bin/env python3
"""Fuzz a running vcad.app's agent bridge (MCP over HTTP on 127.0.0.1).

Targets ONE instance by pid: `VCAD_APP_PID=<pid> python3 -I fuzz/bridge_fuzz.py`
(reads ~/.vcad/instances/<pid>.json). Sends raw-HTTP garbage and random /
malformed tool calls; after every request checks the process is alive and
answers `tools/list` within 15 s. Read-only on disk except a log you pass via
VCAD_FUZZ_LOG. Never point it at an instance you didn't launch.
"""
import json, os, random, socket, sys, time, urllib.request

pid = int(os.environ["VCAD_APP_PID"])
rec = json.load(open(os.path.expanduser(f"~/.vcad/instances/{pid}.json")))
PORT, URL = rec["port"], rec["url"]
N = int(os.environ.get("VCAD_FUZZ_N", "400"))
rng = random.Random(int(os.environ.get("VCAD_FUZZ_SEED", "1")))
SKIP = {  # tools with side effects outside this process, or that block on UI
    "export_cad", "export_gerber", "export_kicad", "export_video", "save_document",
    "fab_prep", "open_in_app", "place_order", "send_to_printer", "print", "mill",
    "quit", "close_window",
    *os.environ.get("VCAD_FUZZ_SKIP", "").split(","),
}
session = None
rid = 0


def post(body: bytes, timeout=60):
    global session
    req = urllib.request.Request(URL, data=body, method="POST")
    req.add_header("Content-Type", "application/json")
    req.add_header("Accept", "application/json, text/event-stream")
    if session:
        req.add_header("Mcp-Session-Id", session)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            session = r.headers.get("Mcp-Session-Id") or session
            return r.status, r.read()
    except urllib.error.HTTPError as e:
        return e.code, e.read()


def rpc(method, params=None, timeout=60):
    global rid
    rid += 1
    msg = {"jsonrpc": "2.0", "id": rid, "method": method}
    if params is not None:
        msg["params"] = params
    st, raw = post(json.dumps(msg).encode(), timeout)
    txt = raw.decode("utf-8", "replace")
    if txt.startswith("event:") or "\ndata:" in txt or txt.startswith("data:"):
        txt = "\n".join(l[5:] for l in txt.splitlines() if l.startswith("data:"))
    try:
        return st, json.loads(txt) if txt.strip() else None
    except json.JSONDecodeError:
        return st, txt


def alive(ctx):
    try:
        os.kill(pid, 0)
    except OSError:
        print(f"CRASH: pid {pid} gone after {ctx}"); sys.exit(2)
    t = time.time()
    try:
        rpc("tools/list", timeout=15)
    except Exception as e:
        print(f"HANG/DEAD: tools/list failed after {ctx}: {e!r} ({time.time()-t:.1f}s)"); sys.exit(3)


def raw_garbage():
    s = socket.create_connection(("127.0.0.1", PORT), timeout=5)
    kind = rng.randrange(6)
    payloads = [
        b"POST /mcp HTTP/1.1\r\nHost: x\r\nContent-Length: 99999999\r\n\r\n{",
        b"POST /mcp HTTP/1.1\r\nHost: x\r\nContent-Length: -5\r\n\r\n",
        b"GET /" + b"A" * 70000 + b" HTTP/1.1\r\n\r\n",
        b"\x00\xff" * 500,
        b"POST /mcp HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\nzzzz\r\n",
        b"POST /mcp HTTP/1.1\r\nContent-Length: 4\r\n\r\n[[[[",
    ]
    try:
        s.sendall(payloads[kind])
        s.settimeout(3)
        s.recv(4096)
    except Exception:
        pass
    finally:
        s.close()
    return f"raw#{kind}"


def junk(depth=0):
    c = rng.randrange(10 if depth < 3 else 8)
    if c == 8:
        return [junk(depth + 1)]
    if c == 9:
        return {"k": junk(depth + 1)}
    return [None, True, -1, 1e308, "", "x" * rng.choice([1, 10, 5000]),
            rng.uniform(-1e3, 1e3), "999:999"][c]


def mutate_args(schema):
    props = (schema or {}).get("properties", {})
    out = {}
    for k, p in props.items():
        if rng.random() < 0.25:
            continue
        t = p.get("type")
        r = rng.random()
        if r < 0.3:
            out[k] = junk()
        elif t == "string":
            out[k] = rng.choice(["", "cube", "1:0", "../../etc/passwd", "\u0000", "é" * 50])
        elif t in ("number", "integer"):
            out[k] = rng.choice([0, -1, 1, 1e9, 3.5, -1e-9])
        elif t == "boolean":
            out[k] = rng.random() < 0.5
        elif t == "array":
            out[k] = [junk() for _ in range(rng.randrange(4))]
        else:
            out[k] = junk()
    if rng.random() < 0.1:
        out["unexpected_field"] = junk()
    return out


st, init = rpc("initialize", {"protocolVersion": "2025-06-18", "capabilities": {},
                              "clientInfo": {"name": "vcad-bridge-fuzz", "version": "0"}})
post(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}).encode())
st, tl = rpc("tools/list")
tools = [t for t in tl["result"]["tools"] if t["name"] not in SKIP]
print(f"instance pid={pid} port={PORT}: {len(tools)} tools")
errors = 0
for i in range(N):
    r = rng.random()
    if r < 0.08:
        ctx = raw_garbage()
    elif r < 0.16:
        body = rng.choice([b"", b"{", b"null", b"[]", b'{"jsonrpc":"2.0","id":1}',
                           b'{"jsonrpc":"2.0","id":1,"method":"tools/call","params":7}',
                           json.dumps([{"jsonrpc": "2.0", "id": k, "method": "tools/list"} for k in range(50)]).encode()])
        post(body); ctx = f"body {body[:40]!r}"
    else:
        t = rng.choice(tools)
        args = mutate_args(t.get("inputSchema"))
        ctx = f"tools/call {t['name']} {json.dumps(args)[:300]}"
        t0 = time.time()
        try:
            st, res = rpc("tools/call", {"name": t["name"], "arguments": args}, timeout=60)
        except Exception as e:
            print(f"TIMEOUT/ERR {e!r} ({time.time()-t0:.0f}s): {ctx}"); errors += 1
            alive(ctx); continue
        if time.time() - t0 > 20:
            print(f"SLOW {time.time()-t0:.0f}s: {ctx}")
        if st >= 500:
            print(f"HTTP {st}: {ctx}: {str(res)[:200]}"); errors += 1
    alive(ctx)
    if i % 50 == 0:
        print(f"{i}/{N} ok")
print(f"done: {N} requests, {errors} server errors, instance alive")
