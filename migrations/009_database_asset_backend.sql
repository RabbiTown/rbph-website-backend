CREATE TABLE rb_asset_file_blob (
    file_id     INT PRIMARY KEY REFERENCES rb_asset_file(id) ON DELETE CASCADE,
    content     BYTEA NOT NULL,
    CONSTRAINT rb_ck_asset_file_blob_size
        CHECK (octet_length(content) <= 1048576)
);
