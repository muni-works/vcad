/**
 * The only Supabase code left in the web app: sign in to an existing vcad.io
 * account and read its documents for `/export`. Nothing here writes rows,
 * syncs, shares or creates accounts — the hosted app is retired and this is
 * the off-ramp (docs/launch/web-retirement.md).
 *
 * Sign-in is a full-page OAuth / magic-link redirect back to `/export`
 * (`https://vcad.io/**` is in the project's redirect allow-list, see
 * supabase/config.toml). A session saved by the old web app — including an
 * anonymous one, which also owns rows — is picked up from localStorage under
 * the same default storage key, so those users can export without signing in.
 */

import { useEffect, useState } from "react";
import { createClient, type Session, type SupabaseClient } from "@supabase/supabase-js";

// The `sb_publishable_*` key is a Supabase publishable key, made to ship in
// client bundles; RLS enforces access server-side. Env vars override.
const DEFAULT_SUPABASE_URL = "https://yteuhwciuxcbjwmabawj.supabase.co";
const DEFAULT_SUPABASE_ANON_KEY = "sb_publishable_pt2xNsK8d7fEbdlkj9PQrA_KvYERtjM";

let client: SupabaseClient | null = null;

/** The Supabase client, or null when the build has no credentials. */
export function getSupabase(): SupabaseClient | null {
  if (client) return client;
  const env = import.meta.env as Record<string, string | undefined>;
  const url = env.VITE_SUPABASE_URL || DEFAULT_SUPABASE_URL;
  const key = env.VITE_SUPABASE_ANON_KEY || DEFAULT_SUPABASE_ANON_KEY;
  if (!url || !key) return null;
  client = createClient(url, key, {
    auth: { persistSession: true, autoRefreshToken: true, detectSessionInUrl: true },
  });
  return client;
}

/** Where sign-in redirects come back to. */
function exportRedirectUrl(): string {
  return `${window.location.origin}/export`;
}

/** Current session, kept live across the redirect and sign-out. */
export function useExportSession(): { session: Session | null; loading: boolean } {
  const [session, setSession] = useState<Session | null>(null);
  const [loading, setLoading] = useState(true);
  useEffect(() => {
    const sb = getSupabase();
    if (!sb) {
      setLoading(false);
      return;
    }
    sb.auth.getSession().then(({ data }) => {
      setSession(data.session);
      setLoading(false);
    });
    const { data } = sb.auth.onAuthStateChange((_event, s) => setSession(s));
    return () => data.subscription.unsubscribe();
  }, []);
  return { session, loading };
}

/** Full-page OAuth redirect; returns an error message or null. */
export async function signInWithProvider(provider: "google" | "github"): Promise<string | null> {
  const sb = getSupabase();
  if (!sb) return "Sign-in is not configured on this deployment.";
  const { error } = await sb.auth.signInWithOAuth({
    provider,
    options: { redirectTo: exportRedirectUrl() },
  });
  return error?.message ?? null;
}

/**
 * Email magic link for accounts that signed up by email. `shouldCreateUser`
 * is off: this page signs existing users in, it never creates accounts.
 */
export async function signInWithEmail(email: string): Promise<string | null> {
  const sb = getSupabase();
  if (!sb) return "Sign-in is not configured on this deployment.";
  const { error } = await sb.auth.signInWithOtp({
    email,
    options: { emailRedirectTo: exportRedirectUrl(), shouldCreateUser: false },
  });
  return error?.message ?? null;
}

export async function signOut(): Promise<void> {
  await getSupabase()?.auth.signOut();
}
