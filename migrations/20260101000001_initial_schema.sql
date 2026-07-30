-- FlagForge initial schema.
--
-- Hierarchy: organization -> project -> environment -> flag configuration.
-- A flag is defined once per project (key, variants) and configured
-- independently per environment, which is what makes "on in staging, 5 % in
-- production" a first-class concept rather than two copies of a flag.

-- ---------------------------------------------------------------- tenancy --

CREATE TABLE organizations (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name       TEXT        NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    slug       TEXT        NOT NULL CHECK (slug ~ '^[a-z0-9][a-z0-9-]{0,62}$'),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX organizations_slug_key ON organizations (slug);

CREATE TABLE users (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    organization_id UUID        NOT NULL REFERENCES organizations (id) ON DELETE CASCADE,
    email           TEXT        NOT NULL,
    -- Argon2id PHC string; never a raw password, never a reversible hash.
    password_hash   TEXT        NOT NULL,
    role            TEXT        NOT NULL CHECK (role IN ('owner', 'admin', 'member', 'viewer')),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Login is by email alone, so it has to be unique across tenants. Stored
-- lower-cased by the application; the index enforces it regardless.
CREATE UNIQUE INDEX users_email_key ON users (lower(email));
CREATE INDEX users_organization_id_idx ON users (organization_id);

-- --------------------------------------------------------------- projects --

CREATE TABLE projects (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    organization_id UUID        NOT NULL REFERENCES organizations (id) ON DELETE CASCADE,
    key             TEXT        NOT NULL CHECK (key ~ '^[a-zA-Z0-9._-]{1,128}$'),
    name            TEXT        NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    description     TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX projects_org_key_key ON projects (organization_id, key);

CREATE TABLE environments (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id    UUID        NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    key           TEXT        NOT NULL CHECK (key ~ '^[a-zA-Z0-9._-]{1,128}$'),
    name          TEXT        NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    -- Bucketing salt. Per environment on purpose: without it, a user unlucky
    -- enough to land outside a staging rollout lands outside the production
    -- one too, and canaries stop being independent samples.
    salt          TEXT        NOT NULL,
    is_production BOOLEAN     NOT NULL DEFAULT FALSE,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX environments_project_key_key ON environments (project_id, key);

-- ------------------------------------------------------------------ flags --

CREATE TABLE flags (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id  UUID        NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    key         TEXT        NOT NULL CHECK (key ~ '^[a-zA-Z0-9._-]{1,128}$'),
    name        TEXT        NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    description TEXT,
    -- [{"key": "on", "value": true}, ...] — shared by every environment so a
    -- variant cannot mean one thing in staging and another in production.
    variants    JSONB       NOT NULL,
    archived    BOOLEAN     NOT NULL DEFAULT FALSE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX flags_project_key_key ON flags (project_id, key);
CREATE INDEX flags_project_active_idx ON flags (project_id) WHERE NOT archived;

CREATE TABLE flag_configs (
    flag_id        UUID        NOT NULL REFERENCES flags (id) ON DELETE CASCADE,
    environment_id UUID        NOT NULL REFERENCES environments (id) ON DELETE CASCADE,
    enabled        BOOLEAN     NOT NULL DEFAULT FALSE,
    off_variant    TEXT        NOT NULL,
    -- Serialized flagforge_core::Distribution.
    fallthrough    JSONB       NOT NULL,
    -- Serialized Vec<flagforge_core::Rule>, order-significant.
    rules          JSONB       NOT NULL DEFAULT '[]'::JSONB,
    -- Bumped by trigger on every write; returned with each evaluation so a
    -- client can tell which configuration produced a decision.
    version        BIGINT      NOT NULL DEFAULT 1,
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (flag_id, environment_id)
);

-- The snapshot loader reads every config for one environment; this is the
-- index that keeps that a single index scan.
CREATE INDEX flag_configs_environment_idx ON flag_configs (environment_id);

-- --------------------------------------------------------------- sdk keys --

CREATE TABLE api_keys (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    environment_id UUID        NOT NULL REFERENCES environments (id) ON DELETE CASCADE,
    name           TEXT        NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    -- SHA-256 of the secret. Lookup happens on every SDK request, so this is
    -- deliberately a fast hash over a 256-bit random secret rather than a
    -- password KDF: there is no low-entropy input to protect against guessing.
    key_hash       TEXT        NOT NULL,
    -- Human-recognisable prefix ("ff_srv_a1b2c3d4") shown in the UI so a key
    -- can be identified without ever storing the secret.
    prefix         TEXT        NOT NULL,
    scope          TEXT        NOT NULL CHECK (scope IN ('server', 'client')),
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_used_at   TIMESTAMPTZ,
    revoked_at     TIMESTAMPTZ
);

CREATE UNIQUE INDEX api_keys_hash_key ON api_keys (key_hash);
CREATE INDEX api_keys_environment_idx ON api_keys (environment_id);

-- -------------------------------------------------------------- audit log --

CREATE TABLE audit_log (
    id              BIGSERIAL PRIMARY KEY,
    organization_id UUID        NOT NULL REFERENCES organizations (id) ON DELETE CASCADE,
    -- Nullable: the actor may have been deleted, and losing the user must not
    -- lose the record of what they did.
    actor_user_id   UUID        REFERENCES users (id) ON DELETE SET NULL,
    actor_email     TEXT        NOT NULL,
    action          TEXT        NOT NULL,
    resource_type   TEXT        NOT NULL,
    resource_id     TEXT        NOT NULL,
    environment_id  UUID        REFERENCES environments (id) ON DELETE SET NULL,
    before          JSONB,
    after           JSONB,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX audit_log_org_time_idx ON audit_log (organization_id, created_at DESC, id DESC);
CREATE INDEX audit_log_resource_idx ON audit_log (resource_type, resource_id);
