# `vcad://` URL scheme

The native Mac app (`apple/VcadApp`) registers `vcad` in its bundle
`CFBundleURLTypes` (written by `apple/VcadApp/bundle.sh`). macOS delivers a
link to `AppDelegate.application(_:open:)`, which resolves it through
`AppInstance.resolveDeepLink` / `DeepLink` (`AppInstance.swift`) and then opens
the result like any other document: in the current window if it is an untouched
scratch, otherwise in a new app instance.

## Format

Every link is `vcad://open?<params>`. Exactly one source parameter is used, in
this order of precedence:

| Param | Meaning |
|---|---|
| `path=<absolute path>` | Open a local file. Must be absolute and end in `.vcad`, `.loon` or `.dxf`; must exist. |
| `session=<id>` | Open an MCP session that a **local** server persisted to `~/.vcad/mcp-sessions/<id>.json`. The id matches `^[A-Za-z0-9_-]{1,128}$`. |
| `dir=<absolute dir>` | With `session`: the session directory when the server ran with a non-default `VCAD_MCP_SESSION_DIR`. |
| `doc=<payload>` | The document inline: `.vcad` **JSON**, gzip-compressed, base64url-encoded (no padding). Same encoding as the old vcad.io `#/new?doc=` links, but JSON rather than VCode. |
| `name=<label>` | Optional display name / file name for `session` and `doc`. |

Examples:

```
vcad://open?path=/Users/me/parts/bracket.vcad
vcad://open?session=doc_1_BY6IdzazVTpDToxm&name=Bracket
vcad://open?doc=H4sIAAAAAAAAE6tWKkstKs7Mz1OyUjLQM1TSUcrLT0ktVrKqrtVRKsrPLwEyo2NrARNWNg0nAAAA&name=Shared
```

## Behavior and safety

- `session` and `doc` documents are copied to a private hand-off file
  (`$TMPDIR/vcad-handoff/<uuid>/<name>.vcad`) before opening, so a later Save
  never writes into the MCP server's session store.
- A link is untrusted input (any web page can fire one). The app refuses
  relative paths, non-document extensions, path-like session ids, and inline
  payloads that are not a JSON object. A refused link logs to stderr and opens
  nothing.
- Unknown hosts (`vcad://anything-but-open`) are ignored, which leaves room for
  future verbs.

## Producers

- MCP `open_in_app` (and its legacy alias `open_in_browser`) in
  `packages/mcp/src/tools/share.ts`: local disk-store sessions travel by id,
  everything else inline. The result also links `https://vcad.io/download` as
  the fallback for users without the app.
- The vcad.io retirement pages (`packages/app/src/lib/retirement.ts`) convert
  old `#/new?doc=` / `?ir=` links and public shares into `vcad://open?doc=…`.
