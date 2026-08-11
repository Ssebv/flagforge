-- A/B experiments: a flag measured against a conversion metric.
--
-- An experiment is scoped to one environment, like a segment and for the same
-- reason: production traffic is the population being measured, and staging
-- exposures mixed into it would answer a question nobody asked. Assignment
-- needs no table of its own — the engine's deterministic bucketing already
-- decides who sees which variant — so what is stored is the definition and
-- the tallies.

CREATE TABLE experiments (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    environment_id  UUID        NOT NULL REFERENCES environments (id) ON DELETE CASCADE,
    -- RESTRICT rather than CASCADE: deleting a flag must not silently discard
    -- an experiment's results. The application refuses first with a message
    -- naming the experiments; this is the backstop.
    flag_id         UUID        NOT NULL REFERENCES flags (id) ON DELETE RESTRICT,
    key             TEXT        NOT NULL CHECK (key ~ '^[a-zA-Z0-9._-]{1,128}$'),
    name            TEXT        NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    description     TEXT,
    -- Conversion events carrying this metric key count toward the experiment.
    metric_key      TEXT        NOT NULL CHECK (metric_key ~ '^[a-zA-Z0-9._-]{1,128}$'),
    -- The baseline variant. Validated against the flag's variants by the
    -- application; the flag's variant list lives in JSONB where a CHECK
    -- cannot reach it.
    control_variant TEXT        NOT NULL,
    -- draft -> running -> stopped, one way. A stopped experiment cannot
    -- restart: its counters describe one measurement window, and reopening
    -- the window would average two populations into an answer about neither.
    state           TEXT        NOT NULL DEFAULT 'draft'
                                CHECK (state IN ('draft', 'running', 'stopped')),
    started_at      TIMESTAMPTZ,
    stopped_at      TIMESTAMPTZ,
    -- Bumped by trigger on every write. Running experiments fold into the
    -- snapshot version, so starting or stopping one is visible to SDKs.
    version         BIGINT      NOT NULL DEFAULT 1,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX experiments_environment_key_key ON experiments (environment_id, key);

-- The snapshot loader wants only the running ones, the same shape of partial
-- index flags_project_active_idx uses for unarchived flags.
CREATE INDEX experiments_running_idx ON experiments (environment_id) WHERE state = 'running';

-- Reusing the segments trigger contract: monotonic per row, set by the
-- database so racing writers cannot publish the same version twice.
CREATE TRIGGER experiments_touch
    BEFORE INSERT OR UPDATE ON experiments
    FOR EACH ROW
EXECUTE FUNCTION flagforge_touch_segment();

-- Experiments live on environment_id, so the existing notify function reads
-- the right column.
CREATE TRIGGER experiments_notify
    AFTER INSERT OR UPDATE OR DELETE ON experiments
    FOR EACH ROW
EXECUTE FUNCTION flagforge_notify_change();

-- Pre-aggregated tallies: one row per (variant, kind, hour), incremented in
-- place. The table is bounded by hours elapsed times variants, not by
-- traffic — the write path a raw event log would need is exactly the
-- infrastructure this schema exists to avoid. The price, accepted knowingly,
-- is that individual events are gone: there is nothing to re-analyse later.
CREATE TABLE experiment_counters (
    experiment_id UUID        NOT NULL REFERENCES experiments (id) ON DELETE CASCADE,
    variant       TEXT        NOT NULL,
    kind          TEXT        NOT NULL CHECK (kind IN ('exposure', 'conversion')),
    hour          TIMESTAMPTZ NOT NULL,
    count         BIGINT      NOT NULL CHECK (count >= 0),
    PRIMARY KEY (experiment_id, variant, kind, hour)
);

-- No touch or notify triggers here, deliberately. Counter increments arrive
-- continuously while an experiment runs; a notify per increment would turn
-- the ingest path into a snapshot-cache invalidation storm, and nothing an
-- SDK evaluates depends on a tally.
