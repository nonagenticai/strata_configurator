-- strata-configurator phase-1 core schema (spec section 10.4).
-- Layered objects live in `overlay_row`; every row belongs to exactly one `layer`.

CREATE TABLE tenant (
    id          uuid        PRIMARY KEY
                            CHECK (id <> '00000000-0000-0000-0000-000000000000'),
    name        text        NOT NULL CHECK (name <> ''),
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TYPE member_role AS ENUM ('viewer', 'editor', 'approver', 'admin');

CREATE TABLE member (
    tenant      uuid        NOT NULL REFERENCES tenant (id) ON DELETE CASCADE,
    sub         text        NOT NULL CHECK (sub <> ''),
    role        member_role NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant, sub)
);

-- A layer key (spec 4.1). The CHECKs mirror `LayerKey::validate`: required keys
-- present, every other key absent, no empty strings, a non-nil tenant.
CREATE TABLE layer (
    id               uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    scope            text        NOT NULL
                                 CHECK (scope IN ('base', 'profile', 'industry', 'industry_dept', 'job', 'tenant')),
    industry         text        CHECK (industry <> ''),
    department       text        CHECK (department <> ''),
    job              text        CHECK (job <> ''),
    rule             text        CHECK (rule <> ''),
    dimension_key    text        CHECK (dimension_key <> ''),
    dimension_value  text        CHECK (dimension_value <> ''),
    tenant           uuid        REFERENCES tenant (id)
                                 CHECK (tenant <> '00000000-0000-0000-0000-000000000000'),
    created_at       timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT layer_key_unique UNIQUE NULLS NOT DISTINCT
        (scope, industry, department, job, rule, dimension_key, dimension_value, tenant),
    CONSTRAINT layer_dimension_pair CHECK ((dimension_key IS NULL) = (dimension_value IS NULL)),
    CONSTRAINT layer_scope_keys CHECK (
        CASE scope
            WHEN 'base' THEN industry IS NULL AND department IS NULL AND job IS NULL
                AND rule IS NULL AND dimension_key IS NULL AND tenant IS NULL
            WHEN 'profile' THEN rule IS NOT NULL AND industry IS NULL AND department IS NULL
                AND job IS NULL AND dimension_key IS NULL AND tenant IS NULL
            WHEN 'industry' THEN industry IS NOT NULL AND department IS NULL AND job IS NULL
                AND rule IS NULL AND dimension_key IS NULL AND tenant IS NULL
            WHEN 'industry_dept' THEN industry IS NOT NULL AND department IS NOT NULL
                AND job IS NULL AND rule IS NULL AND dimension_key IS NULL AND tenant IS NULL
            WHEN 'job' THEN job IS NOT NULL AND industry IS NULL AND department IS NULL
                AND rule IS NULL AND tenant IS NULL
            WHEN 'tenant' THEN tenant IS NOT NULL AND industry IS NULL AND department IS NULL
                AND job IS NULL AND rule IS NULL AND dimension_key IS NULL
            ELSE false
        END
    )
);

-- One revision of one layered object. Exactly one revision per (layer, kind, key)
-- is current.
CREATE TABLE overlay_row (
    id          uuid        PRIMARY KEY,
    layer_id    uuid        NOT NULL REFERENCES layer (id),
    kind        text        NOT NULL CHECK (kind IN ('entity', 'attribute', 'relation', 'job')),
    key         text        NOT NULL CHECK (key <> ''),
    op          text        NOT NULL CHECK (op IN ('add', 'patch', 'tombstone')),
    fields      jsonb       NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(fields) = 'object'),
    rev         integer     NOT NULL CHECK (rev >= 1),
    is_current  boolean     NOT NULL,
    author      text        NOT NULL CHECK (author <> ''),
    created_at  timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT overlay_row_rev_unique UNIQUE (layer_id, kind, key, rev)
);

CREATE UNIQUE INDEX overlay_row_current_unique
    ON overlay_row (layer_id, kind, key) WHERE is_current;

CREATE TABLE axis (
    key          text        PRIMARY KEY CHECK (key <> ''),
    cardinality  text        NOT NULL CHECK (cardinality IN ('one', 'many')),
    created_at   timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE axis_value (
    axis         text NOT NULL REFERENCES axis (key),
    key          text NOT NULL CHECK (key <> ''),
    label        text NOT NULL CHECK (label <> ''),
    parent       text,
    description  text,
    PRIMARY KEY (axis, key),
    CONSTRAINT axis_value_parent_fk FOREIGN KEY (axis, parent)
        REFERENCES axis_value (axis, key) DEFERRABLE INITIALLY DEFERRED
);

-- An axis rule owns the profile-scope layer whose rows apply when its condition holds.
CREATE TABLE axis_rule (
    key        text    PRIMARY KEY CHECK (key <> ''),
    priority   integer NOT NULL,
    condition  jsonb   NOT NULL,
    layer_id   uuid    NOT NULL UNIQUE REFERENCES layer (id)
);

CREATE TABLE dimension (
    key         text        PRIMARY KEY CHECK (key <> ''),
    label       text        NOT NULL CHECK (label <> ''),
    created_at  timestamptz NOT NULL DEFAULT now()
);

-- `tenant` NULL = a standard value; otherwise a value only that tenant sees.
CREATE TABLE dimension_value (
    id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    dimension  text NOT NULL REFERENCES dimension (key),
    key        text NOT NULL CHECK (key <> ''),
    label      text NOT NULL CHECK (label <> ''),
    tenant     uuid REFERENCES tenant (id),
    CONSTRAINT dimension_value_unique UNIQUE NULLS NOT DISTINCT (dimension, key, tenant)
);

-- `tenant` NULL = a template such as `electricity`.
CREATE TABLE profile (
    id          uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant      uuid        REFERENCES tenant (id),
    name        text        NOT NULL CHECK (name <> ''),
    "values"    jsonb       NOT NULL CHECK (jsonb_typeof("values") = 'object'),
    created_at  timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT profile_unique UNIQUE NULLS NOT DISTINCT (tenant, name)
);

CREATE TABLE seed_version (
    version     integer     PRIMARY KEY CHECK (version >= 1),
    sha256      text        NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    applied_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE audit_event (
    id           bigserial   PRIMARY KEY,
    at           timestamptz NOT NULL DEFAULT now(),
    actor        text        NOT NULL,
    tenant       uuid,
    action       text        NOT NULL,
    object       text        NOT NULL,
    before_hash  text,
    after_hash   text
);

CREATE INDEX audit_event_tenant_at ON audit_event (tenant, at DESC);
