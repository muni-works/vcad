import { useEffect, useState } from "react";
import { fetchPublicDocument, fetchSharedDocument } from "@vcad/auth";
import { cloudContentToFile } from "@/lib/export-all";
import {
  DOWNLOAD_URL,
  EXPORT_PATH,
  appLinkForDocumentText,
  type RetirementRoute,
} from "@/lib/retirement";
import { PageMark, RetirementBanner } from "./RetirementBanner";

const NOTICES: Record<Extract<RetirementRoute, { kind: "notice" }>["reason"], string> = {
  editor: "The web editor has moved to the vcad app for Mac.",
  "cli-auth": "The vcad CLI no longer needs a vcad.io sign-in — it runs locally.",
  authorize: "Spend authorization has retired along with vcad.io billing.",
  profile: "Public profiles have retired with vcad.io accounts.",
  "local-doc":
    "This link pointed at a document saved in your browser. Export it from the export page, then open it in the app.",
  "bad-link": "This shared link could not be read.",
};

const button =
  "inline-flex h-8 w-full items-center justify-center border border-border bg-bg px-3 text-xs text-text hover:bg-hover";

/** Hand-off to the native app, with the download page as the fallback. */
function OpenInApp({ appUrl, name }: { appUrl: string; name?: string }) {
  return (
    <>
      <p className="mb-3 text-xs text-text">
        Open {name ? <span className="text-brand">{name}</span> : "this document"} in vcad.
      </p>
      <a className={`${button} mb-2`} href={appUrl} data-testid="open-in-app">
        Open in vcad
      </a>
      <a className="text-xs text-text-muted underline" href={DOWNLOAD_URL}>
        Don't have the app? Download vcad for Mac
      </a>
    </>
  );
}

/** A public share: fetched while the share RPCs still answer, then handed off. */
function ShareHandoff({ route }: { route: Extract<RetirementRoute, { kind: "share" }> }) {
  const [state, setState] = useState<
    { s: "loading" } | { s: "gone" } | { s: "ready"; name: string; text: string; ext: string }
  >({ s: "loading" });

  useEffect(() => {
    const load = route.token
      ? fetchSharedDocument(route.token)
      : fetchPublicDocument(route.username!, route.slug!);
    load
      .then((doc) => {
        if (!doc) return setState({ s: "gone" });
        const { ext, text } = cloudContentToFile(doc.content);
        setState({ s: "ready", name: doc.name, text, ext });
      })
      .catch(() => setState({ s: "gone" }));
  }, [route]);

  if (state.s === "loading") return <p className="text-xs text-text-muted">Loading shared document…</p>;
  if (state.s === "gone")
    return <p className="text-xs text-text">This share has ended. Ask its owner to send you the file.</p>;

  const fileHref = URL.createObjectURL(new Blob([state.text], { type: "application/json" }));
  return (
    <>
      {state.ext === "vcad" && <OpenInApp appUrl={appLinkForDocumentText(state.text, state.name)} name={state.name} />}
      <a className={`${button} mt-3`} href={fileHref} download={`${state.name}.${state.ext}`}>
        Download {state.name}.{state.ext}
      </a>
    </>
  );
}

/**
 * Post-retirement stand-in for every old vcad.io route (see
 * docs/launch/web-retirement.md). Only mounted when
 * VITE_VCAD_RETIREMENT_REDIRECTS is on.
 */
export function RetirementPage({ route }: { route: Exclude<RetirementRoute, { kind: "keep" | "export" }> }) {
  return (
    <div className="flex min-h-screen items-center justify-center bg-bg p-6 font-mono">
      <div className="w-full max-w-lg border border-border bg-surface p-6">
        <PageMark label="vcad 2" />
        <RetirementBanner />
        {route.kind === "handoff" && <OpenInApp appUrl={route.appUrl} name={route.name} />}
        {route.kind === "share" && <ShareHandoff route={route} />}
        {route.kind === "notice" && (
          <>
            <p className="mb-3 text-xs text-text">{NOTICES[route.reason]}</p>
            <a className={`${button} mb-2`} href={DOWNLOAD_URL}>Download vcad for Mac</a>
            <a className="text-xs text-text-muted underline" href={EXPORT_PATH}>Export your documents</a>
          </>
        )}
      </div>
    </div>
  );
}
