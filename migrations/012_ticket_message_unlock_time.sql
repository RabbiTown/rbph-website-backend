ALTER TABLE rb_message
ADD COLUMN unlock_at TIMESTAMPTZ,
ADD COLUMN unlock_after_seconds INT NOT NULL DEFAULT 0
CHECK (unlock_after_seconds >= 0);
