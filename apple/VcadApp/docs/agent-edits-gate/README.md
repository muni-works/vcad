# Agent edits gate

Live snapshots from the running app, captured over MCP through the stdio shim
(`scripts/vcad-app-mcp.mjs`) by `scripts/agent-edits-gate.mjs`:

    node apple/VcadApp/scripts/agent-edits-gate.mjs apple/VcadApp/scripts/vcad-app-mcp.mjs apple/VcadApp/docs/agent-edits-gate

`1`–`6`: empty Untitled → plate → fillet → hole → move hole → difference → aluminum,
one tool call and one named undo step each ("Claude: Create Bracket", …).
`undo-1`…`undo-6`: the `undo` tool (⌘Z) walking them back one at a time;
each frame matches the forward frame before it, and `undo-6` is the empty canvas.
