# Privacy

**vcad collects nothing automatically.** The Mac app has no telemetry, no
analytics SDK (no Sentry, PostHog, Mixpanel or similar), no accounts, and no
vcad-operated server to send anything to. Your documents stay on your machine.

Feedback only reaches us through channels you control:

| Path | What it sends | Who sends it |
|---|---|---|
| **Report a Bug…** (Help menu, ⇧⌘P palette, Keyboard Shortcuts sheet) | Opens a pre-filled GitHub issue in your browser with: app version, build number, macOS version, chip name (e.g. "Apple M3 Pro") and the active workspace (Design, Electronics, …). No file paths, document names or contents. | You — nothing is posted until you press Submit on GitHub, under your GitHub account, publicly. |
| **Crash / hang prompt** | macOS delivers crash and hang diagnostics to the app on-device via MetricKit. vcad stores them in `~/Library/Application Support/vcad/Diagnostics` and, on the next launch, asks whether to report. "Report…" opens the same GitHub issue with a short summary (exception type, top stack frames, app/OS version); the full diagnostic JSON is copied to your clipboard for you to paste if you choose. Home-directory paths are replaced with `~`. "Not Now" keeps them for later; "Don't ask again" deletes them and stops asking. | You. vcad makes no network call. |
| **Software updates** (Sparkle) | Your Mac fetches `https://vcad.io/desktop/appcast.xml` to check for updates, like any web request (IP address and user agent reach Cloudflare). No system profile is sent. | Automatic update checks, scheduled by Sparkle. |
| **Downloads** | GitHub and Cloudflare count downloads of release files. | The download itself. |
| **vcad.io website** | Cloudflare Web Analytics: cookieless, aggregate page views (page, referrer, country, browser). No cookies, no fingerprinting, no cross-site tracking, so no consent banner. | Your browser, when you visit vcad.io. |
| **MCP server** (`npx -y @vcad/mcp`) | Runs on your machine and talks only to the agent you connect it to. | — |

If any of this changes, this page changes first.
