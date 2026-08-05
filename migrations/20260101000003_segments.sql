-- Reusable targeting segments.
--
-- A segment is a named audience that flag rules reference by key instead of
-- restating its conditions. It is scoped to one environment, like a flag's
-- configuration and for the same reason: "beta testers" in staging is a
-- different set of people from "beta testers" in production, and a segment
-- shared across environments would make the staging list a production
-- liability.

CREATE TABLE segments (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    environment_id UUID        NOT NULL REFERENCES environments (id) ON DELETE CASCADE,
    key            TEXT        NOT NULL CHECK (key ~ '^[a-zA-Z0-9._-]{1,128}$'),
    name           TEXT        NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    description    TEXT,
    -- Context keys that are always, or never, members. JSON arrays of strings;
    -- the application decodes them into sets.
    included       JSONB       NOT NULL DEFAULT '[]'::JSONB,
    excluded       JSONB       NOT NULL DEFAULT '[]'::JSONB,
    -- Serialized Vec<flagforge_core::SegmentRule>. Order is not significant
    -- here — unlike a flag's rules, segment rules are alternatives — but it is
    -- preserved so the dashboard renders them the way they were written.
    rules          JSONB       NOT NULL DEFAULT '[]'::JSONB,
    -- Bumped by trigger on every write, exactly like flag_configs.version.
    -- A snapshot's version is the maximum across both, because editing a
    -- segment changes what every referencing flag serves.
    version        BIGINT      NOT NULL DEFAULT 1,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX segments_environment_key_key ON segments (environment_id, key);

-- The snapshot loader reads every segment for one environment, the same shape
-- of query as flag_configs_environment_idx serves.
CREATE INDEX segments_environment_idx ON segments (environment_id);

-- Version bumping, reusing the flag_configs trigger's contract: monotonic per
-- row, set by the database so two racing writers cannot publish the same
-- version twice.
CREATE OR REPLACE FUNCTION flagforge_touch_segment() RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at := now();
    IF TG_OP = 'UPDATE' THEN
        NEW.version := OLD.version + 1;
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER segments_touch
    BEFORE INSERT OR UPDATE ON segments
    FOR EACH ROW
EXECUTE FUNCTION flagforge_touch_segment();

-- Segments live on environment_id, so the existing notify function already
-- reads the right column.
CREATE TRIGGER segments_notify
    AFTER INSERT OR UPDATE OR DELETE ON segments
    FOR EACH ROW
EXECUTE FUNCTION flagforge_notify_change();
