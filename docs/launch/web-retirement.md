# vcad.io web retirement

vcad 2 ships as a free, open native Mac app plus the open kernel. The vcad.io
web editor retires, and with it cloud sync, accounts, collaboration and
billing. This doc maps every vcad.io route to its post-retirement behavior and
says what is built and what is still a switch to flip.

## Release-blocking: the update feed stays up

Every installed copy of the Mac app checks
**`https://vcad.io/desktop/appcast.xml`** for updates (`SUFeedURL` in
`apple/VcadApp/bundle.sh`). The file is
`packages/app/public/desktop/appcast.xml`, and
`.github/workflows/desktop-release.yml` updates it by opening an appcast PR.
The DMGs it points to are GitHub release assets.

After vcad.io stops serving the web app:

- `/desktop/appcast.xml`, the `/desktop/` download page, and everything else
  under `/desktop/` keep serving **unchanged**. They are static files, so
  Vercel serves them before any SPA rewrite.
- `isProtectedPath` in `src/lib/retirement.ts` is the first check in the
  planner, and `main.tsx` checks it again. No redirect flag can catch
  `/desktop/*`. Tests in `src/test/retirement.test.ts` pin this.
- The vcad.io domain, its Vercel project (or some static host for
  `/desktop/`), and the `desktop-release.yml` appcast PR flow must all
  outlive the web app. Don't delete `packages/app/public/desktop/` when the
  editor code is removed. Move it, keep the URL, and keep the GitHub releases.
- Moving the feed URL requires an app update that ships the new
  `SUFeedURL` first, while the old feed still serves that update.

## What exists now (nothing flipped)

| Piece | Where | State |
|---|---|---|
| Export-all page | `/export` — `packages/app/src/components/ExportAllPage.tsx`, `src/lib/export-all.ts` | **Live once deployed** (additive; users need it before shutdown) |
| Retirement redirects | `src/lib/retirement.ts` + `RetirementPage.tsx`, mounted in `main.tsx` | Behind `VITE_VCAD_RETIREMENT_REDIRECTS=1` — **default off** |
| Shutdown date | `SHUTDOWN_DATE` in `src/lib/retirement.ts` | `null` → "a date to be announced" |
| `vcad://` handler | `apple/VcadApp` (see `vcad-url-scheme.md`) | Ships with the next app build |
| MCP deep links | `open_in_app` / `open_in_browser` | Emit `vcad://`; `VCAD_LEGACY_WEB_LINKS=1` adds the old web link |

### Export page

A signed-in user (including an anonymous Supabase session, which also owns
rows) downloads every row of `documents` they own as one zip, paged 25 at a
time, read-only — plain `SELECT`s under the existing RLS, no new tables, no
migrations. Each file is the payload the app would have saved: CRDT JSON or IR
JSON as `<name>.vcad`, loon documents as `<name>.loon`; duplicate names get
` (2)`; a `manifest.json` maps files to cloud ids and `updated_at`. A second
button exports **this browser's IndexedDB documents**, which never reached the
cloud if sync was off. Dev preview without Supabase: `/export?mock=1`.

## Route map (when the flag is on)

| vcad.io route | Today | After retirement |
|---|---|---|
| `/` (editor) | Web editor | Notice: "moved to the Mac app" → download + export links |
| `/export` | — | Export-all page (always on, unaffected by the flag) |
| `/#/new?doc=…`, `/?ir=…` (inline doc links, incl. every old MCP `open_in_browser` link) | Opens the doc in the editor | Decode in the browser (VCode → JSON) → **Open in vcad** button with `vcad://open?doc=…`, download fallback |
| `/view/<token>` (share link) | Read-only shared doc | Fetch via `get_shared_document` while Supabase is up → Open in vcad + download `.vcad`; after Supabase shutdown → "This share has ended" |
| `/@user/<slug>` (public doc) | Public doc page | Same as share, via `get_public_document` |
| `/@user` (profile) | Profile page | Notice: profiles retired |
| `/~<id>`, `/d/<id>` (local IndexedDB doc) | Opens local doc | Notice pointing at `/export` (only this browser has the doc) |
| `/cli-auth` (`vcad login`) | Device-code sign-in | Notice: CLI runs locally, no sign-in |
| `/authorize/<id>` (MCP `authorize_spend`) | Spend approval | Notice: billing retired |
| `/privacy`, `/terms`, `/security` | Legal pages | Unchanged |
| `/desktop/appcast.xml`, `/desktop/*` | Sparkle feed + download page | **Unchanged, permanently** (see above) |
| Embed | No embed route exists in `packages/app`; inline previews are the MCP viewer served by the MCP server (`packages/mcp/src/viewer.ts`) | Unaffected by this flag |

## Cut-over checklist

1. Set `SHUTDOWN_DATE`, deploy, announce `/export` (banner copy already in
   `RetirementBanner.tsx`).
2. Ship the Mac app build with the `vcad://` handler; make sure
   `https://vcad.io/download` serves it.
3. Set `VITE_VCAD_RETIREMENT_REDIRECTS=1` in the Vercel env and redeploy.
4. Before and after every step, check that `https://vcad.io/desktop/appcast.xml`
   still returns the feed (`curl -fsS … | head`).
5. After the shutdown date: retire `/api/*` (cli-auth, billing, chat, usage),
   then Supabase. Shares then fall to "This share has ended"; `/export` stops
   working, so keep it up until the announced date.
