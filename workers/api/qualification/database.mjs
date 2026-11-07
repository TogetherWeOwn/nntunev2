// Disposable Actions database only; never fall back to a workspace/live DSN.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';

export const database = process.env.QUALIFICATION_DATABASE_URL;
assert.equal(process.env.GITHUB_ACTIONS, 'true');
const url = new URL(database);
assert.equal(url.protocol, 'postgresql:');
assert.ok(['localhost', '127.0.0.1'].includes(url.hostname));
assert.equal(url.port, '5432');
assert.equal(url.pathname, '/nntune_ci');
assert.equal(url.search, '?sslmode=disable');

export function sql(text) {
  try {
    return execFileSync('psql', [database, '-X', '-qAt', '-v', 'ON_ERROR_STOP=1'], {
      input: text, encoding: 'utf8', timeout: 10_000,
      env: { ...process.env, PGOPTIONS: '-c search_path=public,pg_catalog' },
      stdio: ['pipe', 'pipe', 'pipe'],
    }).trim();
  } catch {
    // Driver errors and arguments can contain credentials or row payloads.
    throw new Error('qualification database operation failed');
  }
}

export function seed(flac = false) {
  // Fixed fixtures get separate DB custody. No SQL value comes from a request
  // or arbitrary fixture label; the commit transport uses the third identity.
  assert.ok(flac === false || flac === true || flac === 'commit');
  const commit = flac === 'commit';
  const owner = commit ? 'qualification-commit-owner' : flac ? 'qualification-flac-owner' : 'qualification-owner';
  const job = commit ? 'qualification-commit-job' : flac ? 'qualification-flac-job' : 'qualification-job';
  // Admin content identity is globally unique, independently of owner/job IDs.
  const original = commit ? 'c' : flac ? 'b' : 'a';
  const email = commit ? 'qualification-commit@example.invalid' : flac ? 'qualification-flac@example.invalid' : 'qualification@example.invalid';
  // Shape-valid historical admission custody, not an authenticated upload test.
  // The actual producer creates every plan, dispatch, receipt and attempt row.
  sql(`
    INSERT INTO users (id,email,created_at,role)
      VALUES ('${owner}','${email}',1700000000,'admin');
    INSERT INTO ingest_jobs
      (id,user_id,idempotency_key,accepted_at,status,source,content_sha256,nzb_bytes,file_count,segment_count)
      VALUES ('${job}','${owner}','admin:' || repeat('${original}',64),1700000000,
        'queued','admin',repeat('${original}',64),79,1,1);
    INSERT INTO ingest_originals
      (job_id,content_sha256,nzb_bytes,nzb_ref_encrypted,nzb_ref_envelope_version,nzb_ref_kek_version,
       nzb_ref_dek_wrapped_nonce,nzb_ref_dek_wrapped,nzb_ref_payload_nonce,nzb_ref_payload_format,created_at)
      VALUES ('${job}',repeat('${original}',64),79,decode(repeat('00',48),'hex'),2,1,
        decode(repeat('00',12),'hex'),decode(repeat('00',48),'hex'),decode(repeat('00',12),'hex'),1,1700000000);
  `);
}

export function state(ignoreSendAcknowledged = false) {
  // Fingerprint complete persisted rows without exposing envelope/token bytes.
  // The send-replay control may exclude only the monotonic acknowledgement bit.
  assert.equal(typeof ignoreSendAcknowledged, 'boolean');
  const dispatch = ignoreSendAcknowledged ? "to_jsonb(d) - 'send_acknowledged'" : 'to_jsonb(d)';
  return sql(`SELECT md5(concat_ws('|',
    (SELECT jsonb_agg(${dispatch} ORDER BY id)::text FROM ingest_file_dispatches d),
    (SELECT jsonb_agg(to_jsonb(p) ORDER BY id)::text FROM ingest_preparation_plans p),
    (SELECT jsonb_agg(to_jsonb(a) ORDER BY job_id)::text FROM posted_file_job_attempts a),
    (SELECT jsonb_agg(to_jsonb(r) ORDER BY job_id,request_id)::text FROM posted_file_attempt_receipts r),
    (SELECT jsonb_agg(to_jsonb(j) ORDER BY id)::text FROM ingest_jobs j)));`);
}
