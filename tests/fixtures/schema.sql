PRAGMA application_id=42;
PRAGMA user_version=7;

CREATE TABLE audit(value TEXT);
CREATE TABLE samples(
    z ANY,
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    i8 INT8 DEFAULT -1,
    i16 INT16,
    i32 INT32,
    u8 UINT8,
    u16 UINT16,
    u32 UINT32,
    u64 UINT64,
    amount DECIMAL(12,2),
    arbitrary_amount DECIMAL,
    ratio REAL,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    label TEXT COLLATE text_nocase,
    code VARCHAR(12) CONSTRAINT code_nonempty CHECK(length(code)>0),
    unlimited VARCHAR,
    bytes BYTEA,
    raw BLOB,
    uuid UUID,
    document JSON NOT NULL DEFAULT '{}',
    at INSTANT,
    day DATE,
    local_at DATETIME,
    duration INTERVAL,
    tags TEXT[] NOT NULL DEFAULT '[]',
    matrix UINT8[][],
    amounts DECIMAL(8,2)[],
    derived BOOLEAN GENERATED ALWAYS AS(enabled) STORED,
    folded TEXT AS(text_lower(label)) VIRTUAL,
    CONSTRAINT sample_code_unique UNIQUE(code),
    CHECK(amount IS NULL OR amount>=0)
);
CREATE UNIQUE INDEX sample_label
ON samples(text_lower(label) COLLATE text_nocase DESC) WHERE enabled=TRUE;
CREATE VIEW enabled_samples(sample_id,label) AS
SELECT id,label FROM samples WHERE enabled=TRUE;
CREATE TRIGGER sample_label_changed AFTER UPDATE OF label ON samples
WHEN OLD.label IS NOT NEW.label BEGIN
    INSERT INTO audit VALUES(NEW.label);
END;
