import { DOWNLOAD_URL, shutdownLabel } from "@/lib/retirement";

/** "vcad 2 is here" banner shared by the export and retirement pages. */
export function RetirementBanner() {
  return (
    <div className="mb-5 border border-brand/40 bg-brand/10 p-4" role="status">
      <p className="mb-1 text-sm font-bold text-text">
        vcad 2 is here — a free, open native app for Mac.
      </p>
      <p className="text-xs text-text-muted">
        The vcad.io web app, cloud sync and accounts shut down on{" "}
        <span className="text-text" data-testid="shutdown-date">{shutdownLabel()}</span>.
        Download your documents below, then open them in{" "}
        <a className="text-brand underline" href={DOWNLOAD_URL}>vcad for Mac</a>.
      </p>
    </div>
  );
}

/** The small "vcad." wordmark header used by standalone pages. */
export function PageMark({ label }: { label: string }) {
  return (
    <div className="mb-4 flex items-center gap-2">
      <span className="text-sm font-bold tracking-tighter text-text">
        vcad<span className="text-brand">.</span>
      </span>
      <span className="text-xs text-text-muted">{label}</span>
    </div>
  );
}
