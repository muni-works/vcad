#!/usr/bin/env bash
# MCP-native M1 demo: the agent cursor.
#
# With vcad.app running (Untitled window), this plays an agent session over
# the app's MCP bridge: build a block, select an edge (normally the USER
# clicks it — select_edges stands in), read the selection resource, then
# fillet it inside one named turn. Watch the violet "Claude · …" cursor glide
# to the edge and the fillet land live; ⌘Z rewinds the turn as one step
# ("Undo Agent: rounded the edge"). Screenshots land in $OUT.
set -euo pipefail
URL="${VCAD_APP_URL:-$(python3 -c 'import json,os;print(json.load(open(os.path.expanduser("~/.vcad/app.json")))["url"])')}"
OUT="${OUT:-/tmp/vcad-agent-cursor}"; mkdir -p "$OUT"
n=0
rpc() { n=$((n+1)); curl -s "$URL" -H 'content-type: application/json' -d "{\"jsonrpc\":\"2.0\",\"id\":$n,\"method\":\"$1\",\"params\":$2}"; echo; }
tool() { local args="${2:-}"; [ -n "$args" ] || args='{}'; rpc tools/call "{\"name\":\"$1\",\"arguments\":$args}"; }
shot() {
  sleep "${2:-1.2}"
  tool app_screenshot > "$OUT/.resp.json"
  python3 - "$OUT/.resp.json" "$OUT/$1.png" <<'PY'
import base64, json, sys
d = json.load(open(sys.argv[1]))
imgs = [c for c in d.get("result", {}).get("content", []) if c.get("type") == "image"]
if not imgs: sys.exit(f"no screenshot: {str(d)[:200]}")
open(sys.argv[2], "wb").write(base64.b64decode(imgs[0]["data"]))
PY
  rm -f "$OUT/.resp.json"; echo "  → $OUT/$1.png"
}
rpc initialize '{"protocolVersion":"2026-07-28","clientInfo":{"name":"claude-code","version":"demo"},"capabilities":{}}' >/dev/null
rpc resources/subscribe '{"uri":"vcad://doc/selection"}' >/dev/null
tool create '{"type":"cube","name":"Block","params":{"size":{"x":60,"y":40,"z":20}}}'
shot 1-block 2
# edge 4 = the front-top edge of a fresh 60×40×20 block
tool select_edges '{"part":0,"edges":[4]}'
shot 2-user-selects-edge
rpc resources/read '{"uri":"vcad://doc/selection"}'
tool agent_turn '{"label":"rounded the edge"}'
tool fillet_edges '{"radius":6}'
shot 3-agent-fillets 0.3
shot 4-cursor-landed 1.0
tool agent_turn '{"end":true}'
tool app_state | head -c 400; echo
