-- W4 (configurator plan 03, Tasks 2-3): the four job-content overlay kinds. Each row holds one item of
-- care-bus release bundle v1 (context space, procedure, connector slot, knowledge store) for a job or
-- tenant layer; seed v2 fills them for customer_care_agent. Forward-only (pre-prod).
--
-- 0001 declared the kind CHECK unnamed on the column, so Postgres named it overlay_row_kind_check.
-- overlay_row already has RLS and its policies (0003), so nothing else changes.

ALTER TABLE overlay_row DROP CONSTRAINT overlay_row_kind_check;

ALTER TABLE overlay_row ADD CONSTRAINT overlay_row_kind_check CHECK (
    kind IN (
        'entity', 'attribute', 'relation', 'job',
        'context_space', 'procedure', 'connector_slot', 'knowledge_store'
    )
);
