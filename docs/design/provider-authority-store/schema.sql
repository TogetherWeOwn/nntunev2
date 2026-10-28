-- NON-AUTHORITATIVE / NOT INSTALLED.
-- Definition-only text for the provider authority store records. It is not a
-- migration, is never read by a runner, workflow, test harness or include, and
-- is not proof of grants, locking, races or readiness. Static inspection only.
--
-- Scoping (S 4.4 / R 3 / P2 8-10): every uniqueness key starts with env.
-- Native identities (owner, pool_owner, track_id, session_id, job_id) stay text
-- and are compared byte for byte. No single column ID is globally unique.
-- Immutability of origin, plan and step identity is a writer contract: an
-- installing step (separately authorised) must deny any change to these rows.
-- Raw grants, proofs, keys, provider settings and vault envelopes have no column.
-- Retention: closure plus at least 24 hours (86400 seconds), never shortened.

create table play_origin (
    v smallint not null,
    kind text not null check (kind = 'play'),
    env text not null,
    play_intent_id text not null,
    authorization_id text not null,
    playback_request_id text not null,
    owner text not null,
    pool_owner text not null,
    track_id text not null,
    session_id text not null,
    session_epoch integer not null,
    user_epoch integer not null,
    library_epoch integer not null,
    track_epoch integer not null,
    material_epoch integer not null,
    grant_digest text not null,
    grant_expires_at bigint not null,
    metadata_contract text not null,
    admission_id text not null,
    publication_id text not null,
    manifest_id text not null,
    manifest_revision integer not null,
    manifest_digest text not null,
    source jsonb not null,
    material_ref jsonb not null,
    selection jsonb not null,
    issued_at bigint not null,
    hard_deadline bigint not null,
    plan_digest text not null,
    primary key (env, play_intent_id),
    unique (env, authorization_id),
    unique (env, playback_request_id),
    check (authorization_id <> play_intent_id),
    check (authorization_id <> playback_request_id),
    check (play_intent_id <> playback_request_id),
    check (hard_deadline >= issued_at),
    check (owner = pool_owner)
);

create table probe_origin (
    v smallint not null,
    kind text not null check (kind = 'probe'),
    env text not null,
    job_id text not null,
    attempt_id text not null,
    delivery_id text not null,
    owner text not null,
    pool_owner text not null,
    permission_epoch integer not null,
    user_epoch integer not null,
    admission_id text not null,
    job_generation integer not null,
    source_selection_id text not null,
    selection_version integer not null,
    selection_plan_digest text not null,
    audio_ordinal integer not null,
    track_id text not null,
    manifest_id text not null,
    manifest_revision integer not null,
    input_object_id text not null,
    input_revision integer not null,
    input_digest text not null,
    accepted_projection_id text not null,
    accepted_projection_digest text not null,
    consumer text not null,
    operation text not null,
    issued_at bigint not null,
    hard_deadline bigint not null,
    plan_digest text not null,
    primary key (env, job_id, attempt_id, delivery_id),
    check (hard_deadline >= issued_at),
    check (owner = pool_owner)
);

create table provider_plan (
    env text not null,
    parent_id text not null,
    kind text not null check (kind in ('play', 'probe')),
    actions jsonb not null,
    plan_digest text not null,
    primary key (env, parent_id)
);

create table content_step (
    env text not null,
    parent_id text not null,
    step_id text not null,
    action_ordinal integer not null,
    state text not null check (state in ('pending', 'admitted', 'done', 'failed', 'revoked')),
    primary key (env, parent_id, step_id),
    unique (env, parent_id, action_ordinal)
);

create table connection_step (
    env text not null,
    scope_id text not null,
    step_id text not null,
    pair smallint not null check (pair >= 0 and pair < 50),
    operation text not null check (operation in ('connect', 'checkout')),
    state text not null check (state in ('pending', 'admitted', 'done', 'failed', 'revoked')),
    primary key (env, scope_id, step_id),
    unique (env, scope_id, pair, operation)
);

create table provider_jti (
    env text not null,
    issuer text not null,
    jti text not null,
    claim_digest text not null,
    issuer_key text not null,
    cnf jsonb not null,
    owner_epoch bigint not null,
    permission_epoch bigint not null,
    provider_version bigint not null,
    policy_version bigint not null,
    context_digest text not null,
    scope_id text not null,
    scope_digest text not null,
    plan_digest text not null,
    issued_at bigint not null,
    expires_at bigint not null,
    state text not null check (state in ('frozen', 'reserved', 'consumed', 'failed', 'revoked', 'expired')),
    reservation_id text,
    reservation_until bigint,
    consuming_request_id text,
    lease_until bigint,
    terminal_outcome text,
    audit_event_id text,
    primary key (env, issuer, jti),
    check (reservation_id is null or reservation_until is not null)
);

create table proof_nonce (
    env text not null,
    caller text not null,
    kid text not null,
    kv bigint not null,
    proof_jti text not null,
    recorded_at bigint not null,
    primary key (env, caller, kid, kv, proof_jti)
);

create table provider_reservation (
    env text not null,
    reservation_id text not null,
    jti text not null,
    resolver_role text not null,
    cnf jsonb not null,
    reserved_at bigint not null,
    reservation_until bigint not null,
    state text not null check (state in ('reserved', 'consumed', 'failed', 'revoked', 'expired')),
    primary key (env, reservation_id),
    unique (env, jti),
    check (reservation_until > reserved_at)
);

create table provider_lease (
    env text not null,
    lease_id text not null,
    jti text not null,
    scope_id text not null,
    scope_digest text not null,
    context_digest text not null,
    plan_digest text not null,
    cnf jsonb not null,
    issuer_key text not null,
    lease_until bigint not null,
    state text not null check (state in ('active', 'revoked', 'expired')),
    primary key (env, lease_id),
    unique (env, jti)
);

create table authority_fence (
    env text not null,
    owner text not null,
    generation bigint not null,
    state text not null check (state in ('open', 'fenced')),
    primary key (env, owner)
);

create table authority_tombstone (
    env text not null,
    parent_id text not null,
    closed_at bigint not null,
    retain_until bigint not null,
    replacement_used boolean not null,
    primary key (env, parent_id),
    check (retain_until >= closed_at + 86400)
);

create table audit_outbox (
    event_id text not null primary key,
    environment text not null,
    role text not null,
    purpose text not null,
    contract_version text not null,
    outcome text not null,
    reason text not null,
    timestamp bigint not null,
    request_id text,
    authorization_id text,
    attempt_id text,
    publication_id text,
    revision integer,
    generation integer,
    count_bucket text,
    latency_bucket text
);
