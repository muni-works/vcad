import {
  DOWNLOAD_URL,
  type RetirementRoute,
} from "@/lib/retirement";
import { PageMark, RetirementBanner } from "./RetirementBanner";

const NOTICES: Record<Extract<RetirementRoute, { kind: "notice" }>["reason"], string> = {
  editor: "The web editor has moved to the vcad app for Mac.",
  "cli-auth": "The vcad CLI no longer needs a vcad.io sign-in — it runs locally.",
  authorize: "Spend authorization has retired along with vcad.io billing.",
  profile: "Public profiles have retired with vcad.io accounts.",
  "local-doc":
    "This link pointed at a document saved in your browser. Download it from the web editor's document list, then open it in the app.",
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

/** Share links resolved through vcad.io's hosted database, which is gone. */
function ShareEnded() {
  return (
    <>
      <p className="mb-3 text-xs text-text">This share has ended. Ask its owner to send you the file.</p>
      <a className={button} href={DOWNLOAD_URL}>Download vcad for Mac</a>
    </>
  );
}

/**
 * Post-retirement stand-in for every old vcad.io route (see
 * docs/launch/web-retirement.md). Only mounted when
 * VITE_VCAD_RETIREMENT_REDIRECTS is on.
 */
export function RetirementPage({ route }: { route: Exclude<RetirementRoute, { kind: "keep" }> }) {
  return (
    <div className="flex min-h-screen items-center justify-center bg-bg p-6 font-mono">
      <div className="w-full max-w-lg border border-border bg-surface p-6">
        <PageMark label="vcad 2" />
        <RetirementBanner />
        {route.kind === "handoff" && <OpenInApp appUrl={route.appUrl} name={route.name} />}
        {route.kind === "share" && <ShareEnded />}
        {route.kind === "export" && <ShareEnded />}
        {route.kind === "notice" && (
          <>
            <p className="mb-3 text-xs text-text">{NOTICES[route.reason]}</p>
            <a className={button} href={DOWNLOAD_URL}>Download vcad for Mac</a>
          </>
        )}
      </div>
    </div>
  );
}
