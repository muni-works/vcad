import type { VercelRequest, VercelResponse } from "@vercel/node";
import { createClient } from "@supabase/supabase-js";

// Auth client (anon key) for verifying user tokens
const supabase = createClient(
  process.env.SUPABASE_URL!,
  process.env.SUPABASE_ANON_KEY!
);

// Allowed origins for CORS
const ALLOWED_ORIGINS = [
  "https://vcad.io",
  "https://www.vcad.io",
  "http://localhost:5173",
  "http://localhost:3000",
];

function setCorsHeaders(req: VercelRequest, res: VercelResponse): void {
  const origin = req.headers.origin;
  if (origin && ALLOWED_ORIGINS.includes(origin)) {
    res.setHeader("Access-Control-Allow-Origin", origin);
  }
  res.setHeader("Access-Control-Allow-Methods", "POST, OPTIONS");
  res.setHeader(
    "Access-Control-Allow-Headers",
    "Authorization, Content-Type"
  );
}

export default async function handler(
  req: VercelRequest,
  res: VercelResponse
): Promise<void> {
  // CORS headers
  setCorsHeaders(req, res);

  if (req.method === "OPTIONS") {
    res.status(200).end();
    return;
  }

  if (req.method !== "POST") {
    res.status(405).json({ error: "Method not allowed" });
    return;
  }

  // Verify auth via Supabase JWT
  const authHeader = req.headers.authorization;
  if (!authHeader?.startsWith("Bearer ")) {
    res.status(401).json({ error: "Unauthorized" });
    return;
  }

  const token = authHeader.slice(7);
  const {
    data: { user },
    error: authError,
  } = await supabase.auth.getUser(token);

  if (authError || !user) {
    res.status(401).json({ error: "Unauthorized" });
    return;
  }

  const { logId, rating } = req.body;

  // Validate logId
  if (!logId || typeof logId !== "string") {
    res.status(400).json({ error: "logId required" });
    return;
  }

  // Validate rating (-1, 0, or 1)
  if (typeof rating !== "number" || rating < -1 || rating > 1) {
    res.status(400).json({ error: "rating must be -1, 0, or 1" });
    return;
  }

  // Update the rating using user's auth context (RLS enforces ownership)
  const supabaseUser = createClient(
    process.env.SUPABASE_URL!,
    process.env.SUPABASE_ANON_KEY!,
    {
      global: {
        headers: {
          Authorization: `Bearer ${token}`,
        },
      },
    }
  );

  const { error: updateError } = await supabaseUser
    .from("inference_logs")
    .update({
      rating,
      rated_at: new Date().toISOString(),
    })
    .eq("id", logId);

  if (updateError) {
    console.error("Failed to update rating:", updateError);
    res.status(500).json({ error: "Failed to save rating" });
    return;
  }

  res.status(200).json({ success: true });
}
