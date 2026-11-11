import Foundation

// The MCP Apps viewer (`ui://vcad/viewer`), served from the live window.
//
// It renders no geometry of its own: it asks the app for the real viewport
// (`viewer_frame`) after every tool result and every few seconds, so what the
// chat shows is exactly what the user's window shows — agent cursor
// included. "Open in vcad" calls `open_in_app`, which focuses the existing
// window instead of opening a copy. JSON-RPC over postMessage, per MCP Apps.

extension AgentViewer {
    static let html = #"""
<!doctype html>
<html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width">
<style>
:root{color-scheme:light dark;--bg:#16161a;--fg:#e8e8ee;--mut:#9a9aa8;--acc:#8c6bfa}
body{margin:0;background:var(--bg);color:var(--fg);font:13px -apple-system,system-ui,sans-serif}
#v{display:block;width:100%;aspect-ratio:16/10;object-fit:contain;background:#202024}
footer{display:flex;align-items:center;gap:8px;padding:8px 10px}
#n{flex:1;color:var(--mut);white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
button{background:var(--acc);color:#fff;border:0;border-radius:6px;padding:6px 12px;font-weight:600;cursor:pointer}
</style></head><body>
<img id="v" alt="vcad viewport">
<footer><span id="n">connecting to vcad…</span><button id="o">Open in vcad</button></footer>
<script>
let seq=0;const wait=new Map();
function rpc(method,params){const id=++seq;parent.postMessage({jsonrpc:"2.0",id,method,params},"*");
  return new Promise((ok,no)=>{wait.set(id,{ok,no});setTimeout(()=>{if(wait.delete(id))no("timeout")},15000)})}
addEventListener("message",e=>{const m=e.data;if(!m||m.jsonrpc!=="2.0")return;
  if(m.id&&wait.has(m.id)){const w=wait.get(m.id);wait.delete(m.id);m.error?w.no(m.error):w.ok(m.result);return}
  if(m.method==="ui/notifications/tool-result")refresh()});
let busy=false;
async function refresh(){if(busy)return;busy=true;try{
  const r=await rpc("tools/call",{name:"viewer_frame",arguments:{}});
  const img=(r.content||[]).find(c=>c.type==="image");if(img)v.src="data:image/png;base64,"+img.data;
  const t=(r.content||[]).find(c=>c.type==="text");if(t)n.textContent=t.text}
  catch(e){n.textContent="vcad isn't reachable — is vcad.app running?"}finally{busy=false}}
o.onclick=()=>rpc("tools/call",{name:"open_in_app",arguments:{}}).catch(()=>{});
rpc("ui/initialize",{protocolVersion:"2026-01-26",appInfo:{name:"vcad viewer",version:"1"},appCapabilities:{}})
  .then(()=>{parent.postMessage({jsonrpc:"2.0",method:"ui/notifications/initialized"},"*");refresh()})
  .catch(()=>refresh());
setInterval(refresh,3000);
</script></body></html>
"""#
}
