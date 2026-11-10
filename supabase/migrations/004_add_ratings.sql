-- Add rating columns to inference_logs for user feedback
ALTER TABLE inference_logs
  ADD COLUMN rating SMALLINT CHECK (rating >= -1 AND rating <= 1),
  ADD COLUMN rated_at TIMESTAMPTZ;

-- Index for analytics queries on rated entries
CREATE INDEX IF NOT EXISTS inference_logs_rating_idx
  ON inference_logs(rating)
  WHERE rating IS NOT NULL;

-- Allow users to update their own logs (for rating only)
CREATE POLICY "Users can rate own inference logs"
  ON inference_logs FOR UPDATE
  USING (auth.uid() = user_id);
