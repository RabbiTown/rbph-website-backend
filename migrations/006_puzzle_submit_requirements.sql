ALTER TABLE rb_puzzle
    ADD COLUMN submit_requirements JSONB NOT NULL DEFAULT '[]'::JSONB,
    ALTER COLUMN judge SET DEFAULT '[]'::JSONB;
