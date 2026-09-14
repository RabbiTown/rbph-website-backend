ALTER TABLE rb_hint
ADD COLUMN triggers TEXT[] NOT NULL DEFAULT '{}';

ALTER TABLE rb_team_puzzle_trigger
ADD COLUMN source_hint_id INT REFERENCES rb_hint(id) ON DELETE SET NULL;
