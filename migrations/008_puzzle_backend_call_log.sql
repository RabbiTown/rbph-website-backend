TRUNCATE TABLE rb_puzzle_backend_call_log;

ALTER TABLE rb_puzzle_backend_call_log
    DROP CONSTRAINT rb_puzzle_backend_call_log_team_id_fkey,
    DROP CONSTRAINT rb_puzzle_backend_call_log_user_id_fkey,
    ADD COLUMN execution_type VARCHAR(32) NOT NULL,
    ADD COLUMN request_method VARCHAR(16),
    ADD COLUMN duration_ms BIGINT NOT NULL,
    ADD COLUMN submission_id INT,
    ADD COLUMN hint_id INT,
    ADD COLUMN console JSONB NOT NULL DEFAULT '[]'::JSONB,
    ADD COLUMN console_truncated BOOLEAN NOT NULL DEFAULT FALSE;

ALTER TABLE rb_puzzle_backend_call_log
    ADD CONSTRAINT rb_ck_puzzle_backend_call_log_execution_type
        CHECK (execution_type IN ('api', 'judge', 'hint_purchase')),
    ADD CONSTRAINT rb_ck_puzzle_backend_call_log_duration
        CHECK (duration_ms >= 0),
    ADD CONSTRAINT rb_ck_puzzle_backend_call_log_console
        CHECK (jsonb_typeof(console) = 'array');

CREATE INDEX rb_idx_puzzle_backend_call_log_puzzle_type_status_ctime
ON rb_puzzle_backend_call_log(puzzle_id, execution_type, ok, ctime_at DESC, id DESC);

CREATE INDEX rb_idx_puzzle_backend_call_log_puzzle_function_ctime
ON rb_puzzle_backend_call_log(puzzle_id, function_name, ctime_at DESC, id DESC);

CREATE INDEX rb_idx_puzzle_backend_call_log_puzzle_team_ctime
ON rb_puzzle_backend_call_log(puzzle_id, team_id, ctime_at DESC, id DESC);

CREATE INDEX rb_idx_puzzle_backend_call_log_puzzle_user_ctime
ON rb_puzzle_backend_call_log(puzzle_id, user_id, ctime_at DESC, id DESC);
