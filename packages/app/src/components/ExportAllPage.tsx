import { useState } from "react";
import { useAuth, SignInButton, getSupabase, vcadFileToCloudContent } from "@vcad/auth";
import {
  buildExportZip,
  exportZipName,
  fetchAllDocuments,
  supabaseDocumentFetcher,
  type DocumentPageFetcher,
  type ExportableDocument,
} from "@/lib/export-all";
import { getAllDocuments } from "@/lib/storage";
import { PageMark, RetirementBanner } from "./RetirementBanner";

type Status =
  | { state: "idle" }
  | { state: "working"; fetched: number }
  | { state: "done"; count: number }
  | { state: "error"; message: string };

function download(bytes: Uint8Array, filename: string) {
  const blob = new Blob([bytes as unknown as BlobPart], { type: "application/zip" });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 10_000);
}

/** Documents in this browser's IndexedDB, in the same shape as cloud rows. */
async function localDocuments(): Promise<ExportableDocument[]> {
  const docs = await getAllDocuments();
  return docs.map((d) => ({
    id: d.id,
    name: d.name,
    content: vcadFileToCloudContent(d.document),
    updated_at: new Date(d.modifiedAt).toISOString(),
  }));
}

/** Dev-only stand-in for a signed-in session (`/export?mock=1`). */
const MOCK_DOCS: ExportableDocument[] = [
  { id: "m1", name: "Bracket", content: { version: "0.1", nodes: {}, roots: [] }, updated_at: "2026-10-01T00:00:00Z" },
  { id: "m2", name: "Gearbox lid", content: { replica_id: "r", ops: [] }, updated_at: "2026-09-01T00:00:00Z" },
  { id: "m3", name: "Lamp", content: { loonSource: "[cube 10 10 10]" }, updated_at: "2026-08-01T00:00:00Z" },
];
const mockFetcher: DocumentPageFetcher = async (offset, limit) => {
  await new Promise((r) => setTimeout(r, 150));
  return MOCK_DOCS.slice(offset, offset + limit);
};

function StatusLine({ status, noun }: { status: Status; noun: string }) {
  if (status.state === "working")
    return <p className="mt-2 text-xs text-text-muted">Fetched {status.fetched} {noun}…</p>;
  if (status.state === "done")
    return (
      <p className="mt-2 text-xs text-text" data-testid="export-done">
        {status.count === 0 ? `No ${noun} to export.` : `Downloaded ${status.count} ${noun}.`}
      </p>
    );
  if (status.state === "error")
    return <p className="mt-2 text-xs text-brand">Error: {status.message}</p>;
  return null;
}

/**
 * `/export` — download every document as `.vcad` files in one zip, ahead of
 * the vcad.io shutdown. Read-only: it only SELECTs the caller's rows from the
 * existing `documents` table (RLS scopes them), plus this browser's local
 * documents, which never reached the cloud if sync was off.
 */
export function ExportAllPage() {
  const auth = useAuth();
  const mock = import.meta.env.DEV && new URLSearchParams(window.location.search).has("mock");
  const signedIn = mock || !!auth.session;
  const [cloud, setCloud] = useState<Status>({ state: "idle" });
  const [local, setLocal] = useState<Status>({ state: "idle" });

  async function run(
    set: (s: Status) => void,
    load: (onProgress: (n: number) => void) => Promise<ExportableDocument[]>,
    filename: string,
  ) {
    set({ state: "working", fetched: 0 });
    try {
      const docs = await load((n) => set({ state: "working", fetched: n }));
      if (docs.length > 0) download(buildExportZip(docs), filename);
      set({ state: "done", count: docs.length });
    } catch (err) {
      set({ state: "error", message: err instanceof Error ? err.message : String(err) });
    }
  }

  const exportCloud = () =>
    run(
      setCloud,
      (onProgress) => {
        const supabase = getSupabase();
        if (!mock && !supabase) throw new Error("Cloud sync is not configured on this deployment.");
        const fetcher = mock ? mockFetcher : supabaseDocumentFetcher(supabase!);
        return fetchAllDocuments(fetcher, onProgress);
      },
      exportZipName(),
    );

  const exportLocal = () =>
    run(setLocal, () => localDocuments(), exportZipName().replace("export", "export-browser"));

  const button =
    "h-8 w-full border border-border bg-bg px-3 text-xs text-text hover:bg-hover disabled:opacity-50";

  return (
    <div className="flex min-h-screen items-center justify-center bg-bg p-6 font-mono">
      <div className="w-full max-w-lg border border-border bg-surface p-6">
        <PageMark label="export your documents" />
        <RetirementBanner />

        <section className="mb-5">
          <h2 className="mb-2 text-xs font-bold text-text">Cloud documents</h2>
          {signedIn ? (
            <>
              <p className="mb-3 text-xs text-text-muted">
                Every document synced to your account
                {mock ? " (mock session)" : auth.user?.email ? ` (${auth.user.email})` : ""}, as{" "}
                <code className="text-brand">.vcad</code> files in one zip.
              </p>
              <button className={button} onClick={exportCloud} disabled={cloud.state === "working"}>
                Download all cloud documents (.zip)
              </button>
              <StatusLine status={cloud} noun="documents" />
            </>
          ) : (
            <>
              <p className="mb-3 text-xs text-text">Sign in to download your synced documents.</p>
              <SignInButton className={button} />
            </>
          )}
        </section>

        <section>
          <h2 className="mb-2 text-xs font-bold text-text">Documents in this browser</h2>
          <p className="mb-3 text-xs text-text-muted">
            Documents saved locally on this device, including any that never synced.
          </p>
          <button className={button} onClick={exportLocal} disabled={local.state === "working"}>
            Download this browser's documents (.zip)
          </button>
          <StatusLine status={local} noun="documents" />
        </section>
      </div>
    </div>
  );
}
