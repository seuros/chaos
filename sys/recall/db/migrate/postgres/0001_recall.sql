CREATE EXTENSION IF NOT EXISTS vector WITH SCHEMA public;

CREATE TABLE recall_model (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    fingerprint TEXT NOT NULL CHECK (fingerprint <> '')
);

CREATE TABLE recall_docs (
    scope_kind TEXT NOT NULL CHECK (scope_kind IN ('global', 'project', 'session')),
    scope_id TEXT NOT NULL,
    id TEXT NOT NULL CHECK (btrim(id) <> ''),
    content TEXT NOT NULL CHECK (btrim(content) <> ''),
    metadata JSONB NOT NULL DEFAULT '{}',
    embedding vector(256) NOT NULL,
    model_fingerprint TEXT NOT NULL CHECK (model_fingerprint <> ''),
    revision TEXT NOT NULL DEFAULT gen_random_uuid()::text,
    sources JSONB NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(sources) = 'array'),
    automatic BOOLEAN NOT NULL DEFAULT FALSE,
    charge DOUBLE PRECISION NOT NULL DEFAULT 0.5 CHECK (charge BETWEEN 0 AND 1),
    created_at TIMESTAMPTZ NOT NULL DEFAULT statement_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT statement_timestamp(),
    last_used_at TIMESTAMPTZ,
    PRIMARY KEY (scope_kind, scope_id, id),
    CHECK ((scope_kind = 'global' AND scope_id = '') OR
           (scope_kind <> 'global' AND btrim(scope_id) <> ''))
);
CREATE INDEX recall_docs_hnsw ON recall_docs USING hnsw (embedding vector_cosine_ops);
CREATE INDEX recall_docs_lexical ON recall_docs USING gin (to_tsvector('simple', content));

CREATE TABLE recall_receipts (
    id TEXT PRIMARY KEY DEFAULT gen_random_uuid()::text,
    owner TEXT NOT NULL CHECK (owner <> ''),
    generation TEXT NOT NULL CHECK (generation <> ''),
    expires_at BIGINT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);
CREATE INDEX recall_receipts_owner ON recall_receipts (owner, created_at);
CREATE INDEX recall_receipts_expiry ON recall_receipts (expires_at);

CREATE TABLE recall_selections (
    receipt_id TEXT NOT NULL REFERENCES recall_receipts(id) ON DELETE CASCADE,
    scope_kind TEXT NOT NULL,
    scope_id TEXT NOT NULL,
    id TEXT NOT NULL,
    revision TEXT NOT NULL,
    applied_charge DOUBLE PRECISION CHECK (applied_charge BETWEEN 0 AND 1),
    PRIMARY KEY (receipt_id, scope_kind, scope_id, id)
);

CREATE FUNCTION recall_check_model(p_fingerprint TEXT) RETURNS BOOLEAN
LANGUAGE sql SET search_path FROM CURRENT AS $$
    SELECT EXISTS (SELECT 1 FROM recall_model m
                   WHERE m.singleton AND m.fingerprint = p_fingerprint)
$$;

CREATE FUNCTION recall_bind_model(p_fingerprint TEXT) RETURNS BOOLEAN
LANGUAGE plpgsql SET search_path FROM CURRENT AS $$
BEGIN
    PERFORM pg_advisory_xact_lock(724310621);
    IF EXISTS (SELECT 1 FROM recall_docs d WHERE d.model_fingerprint <> p_fingerprint) THEN
        RETURN FALSE;
    END IF;
    INSERT INTO recall_model (singleton, fingerprint) VALUES (TRUE, p_fingerprint)
    ON CONFLICT (singleton) DO NOTHING;
    RETURN recall_check_model(p_fingerprint);
END
$$;

CREATE FUNCTION recall_store(
    p_kind TEXT, p_scope TEXT, p_id TEXT, p_content TEXT, p_metadata JSONB,
    p_embedding vector(256), p_fingerprint TEXT, p_sources JSONB,
    p_automatic BOOLEAN, p_charge DOUBLE PRECISION
) RETURNS BOOLEAN
LANGUAGE plpgsql SET search_path FROM CURRENT AS $$
BEGIN
    IF NOT recall_check_model(p_fingerprint) THEN
        RETURN FALSE;
    END IF;
    INSERT INTO recall_docs AS d (scope_kind, scope_id, id, content, metadata, embedding,
                             model_fingerprint, sources, automatic, charge)
    VALUES (p_kind, p_scope, p_id, p_content, p_metadata, p_embedding,
            p_fingerprint, p_sources, p_automatic, p_charge)
    ON CONFLICT (scope_kind, scope_id, id) DO UPDATE SET
        content = EXCLUDED.content, metadata = EXCLUDED.metadata,
        embedding = EXCLUDED.embedding, model_fingerprint = EXCLUDED.model_fingerprint,
        revision = EXCLUDED.revision, sources = EXCLUDED.sources,
        automatic = EXCLUDED.automatic, charge = EXCLUDED.charge,
        updated_at = CASE WHEN d.content IS DISTINCT FROM EXCLUDED.content
                          THEN GREATEST(d.updated_at, EXCLUDED.updated_at)
                          ELSE d.updated_at END;
    RETURN TRUE;
END
$$;

CREATE FUNCTION recall_search(
    p_query vector(256), p_kind TEXT, p_scope TEXT, p_global BOOLEAN,
    p_fingerprint TEXT, p_limit BIGINT, p_automatic BOOLEAN
) RETURNS TABLE (
    scope_kind TEXT, scope_id TEXT, id TEXT, content TEXT, metadata JSONB, score REAL,
    revision TEXT, sources JSONB, automatic BOOLEAN, charge DOUBLE PRECISION,
    created_at TIMESTAMPTZ, updated_at TIMESTAMPTZ, last_used_at TIMESTAMPTZ
)
LANGUAGE plpgsql SET search_path FROM CURRENT AS $$
BEGIN
    IF NOT recall_check_model(p_fingerprint) THEN
        RAISE EXCEPTION 'recall model mismatch' USING ERRCODE = 'RC002';
    END IF;
    RETURN QUERY
    SELECT d.scope_kind, d.scope_id, d.id, d.content, d.metadata,
           (1 - (d.embedding <=> p_query))::REAL,
           d.revision, d.sources, d.automatic, d.charge,
           d.created_at, d.updated_at, d.last_used_at
    FROM recall_docs d
    WHERE ((d.scope_kind = p_kind AND d.scope_id = p_scope) OR
           (p_global AND d.scope_kind = 'global' AND d.scope_id = ''))
      AND d.model_fingerprint = p_fingerprint AND (NOT p_automatic OR d.automatic)
    ORDER BY d.embedding <=> p_query, d.scope_kind, d.scope_id, d.id
    LIMIT p_limit;
END
$$;

CREATE FUNCTION recall_lexical_search(
    p_query TEXT, p_kind TEXT, p_scope TEXT, p_global BOOLEAN,
    p_fingerprint TEXT, p_limit BIGINT, p_automatic BOOLEAN
) RETURNS TABLE (
    scope_kind TEXT, scope_id TEXT, id TEXT, content TEXT, metadata JSONB, score REAL,
    revision TEXT, sources JSONB, automatic BOOLEAN, charge DOUBLE PRECISION,
    created_at TIMESTAMPTZ, updated_at TIMESTAMPTZ, last_used_at TIMESTAMPTZ
)
LANGUAGE plpgsql SET search_path FROM CURRENT AS $$
BEGIN
    IF NOT recall_check_model(p_fingerprint) THEN
        RAISE EXCEPTION 'recall model mismatch' USING ERRCODE = 'RC002';
    END IF;
    RETURN QUERY
    SELECT d.scope_kind, d.scope_id, d.id, d.content, d.metadata,
           ts_rank_cd(to_tsvector('simple', d.content), plainto_tsquery('simple', p_query)),
           d.revision, d.sources, d.automatic, d.charge,
           d.created_at, d.updated_at, d.last_used_at
    FROM recall_docs d
    WHERE ((d.scope_kind = p_kind AND d.scope_id = p_scope) OR
           (p_global AND d.scope_kind = 'global' AND d.scope_id = ''))
      AND d.model_fingerprint = p_fingerprint AND (NOT p_automatic OR d.automatic)
      AND to_tsvector('simple', d.content) @@ plainto_tsquery('simple', p_query)
    ORDER BY ts_rank_cd(to_tsvector('simple', d.content), plainto_tsquery('simple', p_query)) DESC,
             d.scope_kind, d.scope_id, d.id
    LIMIT p_limit;
END
$$;

CREATE FUNCTION recall_delete(p_kind TEXT, p_scope TEXT, p_id TEXT) RETURNS void
LANGUAGE sql SET search_path FROM CURRENT AS $$
    DELETE FROM recall_docs d WHERE d.scope_kind = p_kind AND d.scope_id = p_scope AND d.id = p_id
$$;

CREATE FUNCTION recall_lock_owner(p_owner TEXT) RETURNS void
LANGUAGE sql SET search_path FROM CURRENT AS $$
    SELECT pg_advisory_xact_lock(hashtextextended(p_owner, 724310622))
$$;

CREATE FUNCTION recall_issue_receipt(p_owner TEXT, p_generation TEXT, p_selections JSONB)
RETURNS TABLE (id TEXT, expires_at BIGINT)
LANGUAGE plpgsql SET search_path FROM CURRENT AS $$
DECLARE
    v_receipt TEXT;
    v_expires BIGINT;
    v_selection JSONB;
    v_kind TEXT;
    v_scope TEXT;
BEGIN
    IF p_owner IS NULL OR p_owner = '' OR p_generation IS NULL OR p_generation = ''
       OR jsonb_typeof(p_selections) IS DISTINCT FROM 'array' THEN
        RAISE EXCEPTION 'invalid recall receipt' USING ERRCODE = 'RC001';
    END IF;
    IF jsonb_array_length(p_selections) NOT BETWEEN 1 AND 50 THEN
        RAISE EXCEPTION 'invalid recall receipt' USING ERRCODE = 'RC001';
    END IF;
    PERFORM recall_lock_owner(p_owner);
    DELETE FROM recall_receipts r WHERE r.id IN (
        SELECT expired.id FROM recall_receipts expired
        WHERE expired.expires_at <= EXTRACT(EPOCH FROM clock_timestamp())::BIGINT
        ORDER BY expired.expires_at LIMIT 1000
    );
    INSERT INTO recall_receipts AS r (owner, generation, expires_at)
    VALUES (p_owner, p_generation, EXTRACT(EPOCH FROM clock_timestamp())::BIGINT + 3600)
    RETURNING r.id, r.expires_at INTO v_receipt, v_expires;
    FOR v_selection IN
        SELECT value FROM jsonb_array_elements(p_selections)
        ORDER BY value #>> '{scope,kind}', value #>> '{scope,id}', value ->> 'id'
    LOOP
        v_kind := v_selection #>> '{scope,kind}';
        v_scope := COALESCE(v_selection #>> '{scope,id}', '');
        PERFORM 1 FROM recall_docs d
        WHERE d.scope_kind = v_kind AND d.scope_id = v_scope
          AND d.id = v_selection ->> 'id' AND d.revision = v_selection ->> 'revision'
        FOR SHARE;
        IF NOT FOUND THEN
            RAISE EXCEPTION 'invalid recall receipt' USING ERRCODE = 'RC001';
        END IF;
        INSERT INTO recall_selections (receipt_id, scope_kind, scope_id, id, revision)
        VALUES (v_receipt, v_kind, v_scope, v_selection ->> 'id', v_selection ->> 'revision')
        ON CONFLICT DO NOTHING;
    END LOOP;
    DELETE FROM recall_receipts r WHERE r.id IN (
        SELECT older.id FROM recall_receipts older WHERE older.owner = p_owner
        ORDER BY older.created_at DESC, older.id DESC OFFSET 100
    );
    RETURN QUERY SELECT v_receipt, v_expires;
END
$$;

CREATE FUNCTION recall_recalled_memory(
    p_owner TEXT, p_generation TEXT, p_receipt TEXT, p_kind TEXT, p_scope TEXT, p_id TEXT
) RETURNS TABLE (
    revision TEXT, sources JSONB, automatic BOOLEAN, charge DOUBLE PRECISION,
    created_at TIMESTAMPTZ, updated_at TIMESTAMPTZ, last_used_at TIMESTAMPTZ
)
LANGUAGE sql SET search_path FROM CURRENT AS $$
    SELECT d.revision, d.sources, d.automatic, d.charge,
           d.created_at, d.updated_at, d.last_used_at
    FROM recall_receipts r
    JOIN recall_selections s ON s.receipt_id = r.id
    JOIN recall_docs d ON (d.scope_kind, d.scope_id, d.id, d.revision) =
                         (s.scope_kind, s.scope_id, s.id, s.revision)
    WHERE r.id = p_receipt AND r.owner = p_owner AND r.generation = p_generation
      AND r.expires_at > EXTRACT(EPOCH FROM clock_timestamp())::BIGINT
      AND s.scope_kind = p_kind AND s.scope_id = p_scope AND s.id = p_id
$$;

CREATE FUNCTION recall_commit_use(
    p_owner TEXT, p_generation TEXT, p_receipt TEXT, p_kind TEXT, p_scope TEXT, p_id TEXT
) RETURNS TABLE (charge DOUBLE PRECISION, applied BOOLEAN)
LANGUAGE plpgsql SET search_path FROM CURRENT AS $$
DECLARE
    v_charge DOUBLE PRECISION;
    v_previous DOUBLE PRECISION;
BEGIN
    IF p_kind = 'global' THEN
        RAISE EXCEPTION 'invalid recall receipt' USING ERRCODE = 'RC001';
    END IF;
    PERFORM recall_lock_owner(p_owner);
    PERFORM 1 FROM recall_receipts r
    WHERE r.id = p_receipt AND r.owner = p_owner AND r.generation = p_generation
      AND r.expires_at > EXTRACT(EPOCH FROM clock_timestamp())::BIGINT
    FOR UPDATE;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'invalid recall receipt' USING ERRCODE = 'RC001';
    END IF;
    SELECT d.charge, s.applied_charge INTO v_charge, v_previous
    FROM recall_selections s
    JOIN recall_docs d ON (d.scope_kind, d.scope_id, d.id, d.revision) =
                         (s.scope_kind, s.scope_id, s.id, s.revision)
    WHERE s.receipt_id = p_receipt AND s.scope_kind = p_kind
      AND s.scope_id = p_scope AND s.id = p_id
    FOR UPDATE OF s, d;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'invalid recall receipt' USING ERRCODE = 'RC001';
    END IF;
    IF v_previous IS NOT NULL THEN
        RETURN QUERY SELECT v_previous, FALSE;
        RETURN;
    END IF;
    v_charge := LEAST(v_charge + 0.02, 1.0);
    UPDATE recall_docs d SET charge = v_charge,
                            last_used_at = GREATEST(d.last_used_at, clock_timestamp())
    WHERE d.scope_kind = p_kind AND d.scope_id = p_scope AND d.id = p_id;
    UPDATE recall_selections s SET applied_charge = v_charge
    WHERE s.receipt_id = p_receipt AND s.scope_kind = p_kind
      AND s.scope_id = p_scope AND s.id = p_id;
    RETURN QUERY SELECT v_charge, TRUE;
END
$$;
