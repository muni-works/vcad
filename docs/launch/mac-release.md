# vcad for Mac: release pipeline

vcad 1.0 ships as a free, Developer ID–signed, notarized DMG (no App Store),
updated by Sparkle 2 from `https://vcad.io/desktop/appcast.xml`, which is the
static file `packages/app/public/desktop/appcast.xml`. That directory must
outlive everything else on vcad.io.

| Piece | Where |
|---|---|
| Release script | `apple/VcadApp/scripts/release.sh` (local, never publishes) |
| Bundle assembly | `apple/VcadApp/bundle.sh` |
| Entitlements | `apple/VcadApp/VcadApp.entitlements` (`device.audio-input` for Mallet/Tune; nothing else) |
| Updater | `Sources/VcadApp/Updater.swift` ("Check for Updates…" in the vcad menu), `Sources/VcadUpdate/UpdaterConfig.swift` |
| Feed + download page | `packages/app/public/desktop/{appcast.xml,index.html}` |
| DMGs | GitHub Releases `desktop-v<version>` on ecto/vcad (appcast enclosure) |

The older env-driven `apple/VcadApp/release.sh` and the
`desktop-release.yml` CI workflow (see `docs/mac-release.md`) still exist;
`scripts/release.sh` is the local path described here.

## Bundle ids

| Build | `CFBundleIdentifier` |
|---|---|
| Release (`scripts/release.sh`, or `VCAD_RELEASE=1` / `VCAD_SIGN_IDENTITY`) | `io.vcad.m0` (unchanged: shipped apps update in place) |
| Dev (`bundle.sh`) | `io.vcad.dev.<checkout>`, e.g. `io.vcad.dev.main`, `io.vcad.dev.viewport-lighting-bb82bd` |
| Override | `VCAD_BUNDLE_ID=…` |

Each dev checkout gets its own Local Network permission, TCC entries and
computer-use target instead of fighting over `io.vcad.m0`. Dev bundles are
registered with Launch Services (`lsregister -f`) after bundling. All of them
export the same `io.vcad.document` / `io.vcad.loon` UTIs; Finder opens
`.vcad` with whichever owner it picks (the release app once installed in
/Applications). Dev builds keep their own `UserDefaults` domain, so settings
don't carry over from the release app.

## One-time setup (Cam)

Run `apple/VcadApp/scripts/release.sh --dry-run` at any point: its preflight
lists exactly what is still missing, with the fix for each.

### 1. Developer ID certificate: done

`Developer ID Application: Cameron Pedersen (54SNZV8YN8)` is in the login
Keychain. Check: `security find-identity -v -p codesigning`. Override with
`VCAD_SIGN_IDENTITY` (e.g. a later Municipal Robotics team identity).

### 2. Notary profile `vcad` (App Store Connect API key): TODO

The app-specific-password route 401'd for frog, so use an API key:

1. appstoreconnect.apple.com → Users and Access → Integrations → App Store
   Connect API → Team Keys → **+**, name `vcad-notary`, access **Developer**.
2. Download `AuthKey_<KEYID>.p8` (one chance) to `~/keys/` (outside any
   repo, `chmod 600`). Note the Key ID and the Issuer ID on that page.
3. Store the profile in the login Keychain:

   ```bash
   xcrun notarytool store-credentials vcad --key ~/keys/AuthKey_<KEYID>.p8 --key-id <KEYID> --issuer <ISSUER-UUID>
   ```

4. Check: `xcrun notarytool history --keychain-profile vcad`.

If frog's profile was made from a team key with Developer access, the same
key works here (`store-credentials vcad` with it); only the profile name
differs. Override the name with `VCAD_NOTARY_PROFILE`.

### 3. Sparkle EdDSA key: present, confirm it's the one you want

A Sparkle private key already exists in this Mac's login Keychain (item
"https://sparkle-project.org", shared by every Sparkle app on the machine,
so it is probably frog's). Sparkle uses one key per Keychain by default.
Decide:

- **Reuse it** (simplest): nothing to do. `release.sh` reads the public key
  with `generate_keys -p` and bakes it into `SUPublicEDKey`.
- **Separate key for vcad:** `generate_keys --account vcad` creates a second
  one; then the script needs `--account vcad` passed to `generate_keys -p` and
  `sign_update` (small change, not wired yet).

Either way, **the first public release freezes the key**: every installed
copy only accepts updates signed by it. Back it up now:
`.build/artifacts/sparkle/Sparkle/bin/generate_keys -x ~/keys/sparkle-ed25519.key`,
move that file into the password manager, delete it from disk.

The tools live at `apple/VcadApp/.build/artifacts/sparkle/Sparkle/bin/`
after any `swift build`.

### Where each secret lives

| Secret | Location | Never |
|---|---|---|
| Developer ID private key | login Keychain (+ `.p12` backup in password manager) | in the repo, in CI logs |
| App Store Connect API key `.p8` | `~/keys/` + notarytool profile `vcad` in login Keychain | in the repo |
| Sparkle EdDSA private key | login Keychain item `https://sparkle-project.org` (+ `generate_keys -x` backup in password manager) | on disk after backup, in the repo |
| Sparkle EdDSA **public** key | baked into each app's Info.plist at build time | — (it's public) |

## Cutting a release

1. On a clean, up-to-date `main`: bump the version in
   `packages/*/package.json` if needed (the script reads
   `packages/app/package.json`; or pass `release.sh 1.0.0`).
2. Build the static libs: `apple/VcadApp/build-ffi.sh && apple/VcadApp/build-tc-ffi.sh`.
   For the path tracer, build irys first (`~/Developer/irys`, `cargo build --release -p irys-ffi`)
   or set `IRYS_FFI`; without it the DMG ships with Path-Traced View disabled.
3. Dry run, then the real thing:

   ```bash
   apple/VcadApp/scripts/release.sh --dry-run
   ```

   ```bash
   apple/VcadApp/scripts/release.sh
   ```

   It: preflights (identity, notary profile, Sparkle key, tools, libs, clean
   tree, build number above the published one) → `bundle.sh` with
   `CONFIG=release`, `io.vcad.m0`, version + build stamped → signs every
   Mach-O inside-out (irys, Sparkle's XPC services/Autoupdate/Updater.app,
   framework, app with entitlements; hardened runtime + timestamp, never
   `--deep`) → `codesign --verify --deep --strict` + per-binary team/runtime/
   timestamp audit → notarize (`ditto` zip, `submit --wait --timeout 30m`)
   + staple the app → `create-dmg` (540×360, icon 128, app 140,170,
   Applications 400,170) → sign, notarize, staple the DMG → `spctl` on both →
   `sign_update` + verify → one-item `appcast.xml`.
   `--dry-run` does all of it except the two notarizations (so `spctl`
   rejects, which is expected).
4. Output: `apple/VcadApp/dist/release/<version>/` with `vcad-<version>.dmg`,
   its `.sha256`, and `desktop/` mirroring `packages/app/public/desktop/`.
5. Publish (the script prints these; nothing is uploaded automatically):

   ```bash
   gh release create desktop-v<version> apple/VcadApp/dist/release/<version>/vcad-<version>.dmg --repo ecto/vcad --title "vcad <version>" --generate-notes
   ```

   Then copy `desktop/appcast.xml` over `packages/app/public/desktop/appcast.xml`
   in a PR. **Merging it is what ships the update** to installed apps.

`CFBundleVersion` is the git commit count of `HEAD` (monotonic on `main`);
the preflight refuses a number not above the feed's highest
`sparkle:version`. If CI releases (run numbers) were ever mixed in, set
`VCAD_BUILD` above both.
