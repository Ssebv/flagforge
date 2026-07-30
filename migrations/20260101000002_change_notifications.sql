-- Version bumping and cache invalidation.
--
-- Every API node keeps environment snapshots in memory so evaluation never
-- touches the database. Postgres itself tells the nodes when to reload: a
-- trigger emits NOTIFY on every configuration write, and each node LISTENs.
-- Doing it in the database rather than in the application means a change made
-- by a migration, a psql session or a future service still invalidates caches.

CREATE OR REPLACE FUNCTION flagforge_touch_flag_config() RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at := now();
    -- Monotonic per (flag, environment). Callers cannot set it themselves,
    -- so two racing writers can never publish the same version twice.
    IF TG_OP = 'UPDATE' THEN
        NEW.version := OLD.version + 1;
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER flag_configs_touch
    BEFORE INSERT OR UPDATE ON flag_configs
    FOR EACH ROW
EXECUTE FUNCTION flagforge_touch_flag_config();

CREATE OR REPLACE FUNCTION flagforge_notify_change() RETURNS TRIGGER AS $$
DECLARE
    env_id UUID;
BEGIN
    env_id := CASE TG_OP WHEN 'DELETE' THEN OLD.environment_id ELSE NEW.environment_id END;
    -- NOTIFY payloads are capped at 8000 bytes, so we send only the key to
    -- invalidate and let the listener re-read the snapshot it actually needs.
    PERFORM pg_notify('flagforge_env_changed', env_id::TEXT);
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER flag_configs_notify
    AFTER INSERT OR UPDATE OR DELETE ON flag_configs
    FOR EACH ROW
EXECUTE FUNCTION flagforge_notify_change();

-- Editing a flag's variants changes what every environment serves, so it has
-- to invalidate all of that project's environments.
CREATE OR REPLACE FUNCTION flagforge_notify_flag_change() RETURNS TRIGGER AS $$
DECLARE
    env_id     UUID;
    project    UUID;
BEGIN
    project := CASE TG_OP WHEN 'DELETE' THEN OLD.project_id ELSE NEW.project_id END;
    FOR env_id IN SELECT id FROM environments WHERE project_id = project LOOP
        PERFORM pg_notify('flagforge_env_changed', env_id::TEXT);
    END LOOP;
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER flags_notify
    AFTER INSERT OR UPDATE OR DELETE ON flags
    FOR EACH ROW
EXECUTE FUNCTION flagforge_notify_flag_change();

CREATE OR REPLACE FUNCTION flagforge_touch_flag() RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at := now();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER flags_touch
    BEFORE UPDATE ON flags
    FOR EACH ROW
EXECUTE FUNCTION flagforge_touch_flag();
