import { DurableObject } from 'cloudflare:workers';
import { projectTailEvent } from './project-tail-event.mjs';
import { evaluateErrorWindow } from './evaluate-error-window.mjs';
import { renderVerdictAlert } from './render-verdict-alert.mjs';

const PRODUCER = 'nntune-api-staging';
const BATCH_LIMIT = 256;
const OBSERVATION_LIMIT = 2048;
const WINDOW_MS = 60000;
const COOLDOWN_MS = 300000;
const DELIVERY_MS = 5000;
const SHARD_ROWS = 64;
const VALUE_BYTES = 65536;
const STATE_KEY = 'windows:v1';
const FLAGS = ['http_5xx', 'exception', 'exceeded_cpu', 'exceeded_memory'];
const OUTCOMES = ['ok', 'exception', 'exceeded_cpu', 'exceeded_memory', 'unknown'];
const PROJECTION_POLICY = {
  script_name: PRODUCER, service: 'api', environment: 'staging',
  routes: ['GET /health'], error_classes: ['upstream'],
};
const REDUCER_POLICY = {
  service: 'api', environment: 'staging', min_invocations: 20,
  max_error_rate: { failed: 1, total: 20 }, per_error_class: {}, per_release: {},
};

function isRecord(value) {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

// RPC is an independent trust boundary. Rebuild only the bounded v1 wire
// schema, including nested flags; never store caller objects or arbitrary keys.
function canonicalObservation(row) {
  try {
    if (!isRecord(row) || row.schema_version !== 1
      || row.service !== 'api' || row.environment !== 'staging'
      || !OUTCOMES.includes(row.outcome) || !isRecord(row.failure_flags)) return null;
    const flags = {};
    for (const key of FLAGS) {
      if (typeof row.failure_flags[key] !== 'boolean') return null;
      flags[key] = row.failure_flags[key];
    }
    const status = row.http_status;
    if (status !== undefined && (!Number.isInteger(status) || status < 100 || status > 599)) return null;
    if (flags.http_5xx !== (status !== undefined && status >= 500)) return null;
    if ((row.outcome === 'exception' && !flags.exception)
      || flags.exceeded_cpu !== (row.outcome === 'exceeded_cpu')
      || flags.exceeded_memory !== (row.outcome === 'exceeded_memory')) return null;
    if (typeof row.build_sha !== 'string'
      || !(row.build_sha === 'unknown' || /^[0-9a-f]{40}$/.test(row.build_sha))
      || row.release !== row.build_sha) return null;
    if (typeof row.route !== 'string' || row.route.length > 128
      || !(row.route === 'unknown' || /^[A-Z]+ \/[A-Za-z0-9\-._~!$&'()*+,;=:@%/{}]*$/.test(row.route))) return null;
    if (typeof row.error_class !== 'string' || !/^[a-z][a-z0-9_]{0,63}$/.test(row.error_class)) return null;
    return {
      schema_version: 1, service: 'api', environment: 'staging', outcome: row.outcome,
      ...(status === undefined ? {} : { http_status: status }), failure_flags: flags,
      build_sha: row.build_sha, release: row.release, route: row.route, error_class: row.error_class,
    };
  } catch {
    return null;
  }
}

function canonicalBatch(input) {
  const observations = [];
  let incomplete = true;
  try {
    if (!isRecord(input) || !Array.isArray(input.observations) || typeof input.incomplete !== 'boolean') {
      return { observations, incomplete };
    }
    incomplete = input.incomplete || input.observations.length > BATCH_LIMIT;
    for (let i = 0; i < Math.min(input.observations.length, BATCH_LIMIT); i += 1) {
      const row = canonicalObservation(input.observations[i]);
      if (row === null) incomplete = true;
      else observations.push(row);
    }
  } catch {
    incomplete = true;
  }
  return { observations, incomplete };
}

export default {
  async tail(events, env, ctx) {
    if (env.PRODUCER_SCRIPT_NAME !== PRODUCER) return;
    const observations = [];
    let incomplete = !Array.isArray(events);
    if (Array.isArray(events)) {
      incomplete = events.length > BATCH_LIMIT;
      for (let i = 0; i < Math.min(events.length, BATCH_LIMIT); i += 1) {
        try {
          const event = events[i];
          if (event?.scriptName !== PRODUCER) continue;
          const row = projectTailEvent(event, PROJECTION_POLICY);
          if (row === null) incomplete = true;
          else observations.push(row);
        } catch {
          incomplete = true;
        }
      }
    }
    if (!observations.length && !incomplete) return;
    try {
      await env.ALERT_WINDOWS.getByName('api:staging').ingest({ observations, incomplete });
    } catch {
      // No raw binding exception escapes into Tail logs. Failure is lost
      // coverage, not a healthy verdict; platform ingress retries aren't added.
    }
  },
  fetch() {
    return new Response(null, { status: 404 });
  },
};

const shardKey = (window, index) => `rows:${window.start}:${index}`;
const shardKeys = (window) => Array.from({ length: window.shards }, (_, i) => shardKey(window, i));

async function readState(tx) {
  return await tx.get(STATE_KEY) ?? { windows: [], lost: null, cooldownUntil: 0 };
}

async function saveState(tx, state) {
  await tx.put(STATE_KEY, state);
  const deadlines = state.windows.map((window) => window.end);
  if (state.lost !== null) deadlines.push(state.lost.end);
  if (deadlines.length) await tx.setAlarm(Math.min(...deadlines));
  else await tx.deleteAlarm();
}

async function discardRows(tx, window) {
  if (window.shards) await tx.delete(shardKeys(window));
  window.shards = 0;
  window.count = 0;
  window.incomplete = true;
}

async function readRows(tx, window) {
  const shards = await tx.get(shardKeys(window));
  const rows = [];
  for (const key of shardKeys(window)) {
    const shard = shards.get(key);
    if (!Array.isArray(shard) || shard.length > SHARD_ROWS) return null;
    for (const row of shard) {
      const clean = canonicalObservation(row);
      if (clean === null) return null;
      rows.push(clean);
    }
  }
  return rows.length === window.count && rows.length <= OBSERVATION_LIMIT ? rows : null;
}

async function appendRows(tx, window, observations) {
  // Repacking is bounded by 2048 rows and happens within the same transaction.
  // 64 maximal canonical ASCII records fit well below 64KiB; check actual UTF-8
  // bytes too so later schema changes fail closed instead of sampling.
  const previous = await readRows(tx, window);
  if (previous === null) return discardRows(tx, window);
  const rows = previous.concat(observations);
  const shards = [];
  for (let i = 0; i < rows.length; i += SHARD_ROWS) {
    const shard = rows.slice(i, i + SHARD_ROWS);
    if (new TextEncoder().encode(JSON.stringify(shard)).byteLength > VALUE_BYTES) {
      return discardRows(tx, window);
    }
    shards.push(shard);
  }
  for (let i = 0; i < shards.length; i += 1) await tx.put(shardKey(window, i), shards[i]);
  window.shards = shards.length;
  window.count = rows.length;
}

function webhookEndpoint(value) {
  // Validate raw spelling as well as parsed authority: URL normalizes default
  // ports, dot segments and some escapes that are not accepted configuration.
  if (typeof value !== 'string'
    || !/^https:\/\/discord\.com\/api\/webhooks\/[0-9]{1,32}\/[A-Za-z0-9_-]{1,256}$/.test(value)) return null;
  try {
    const url = new URL(value);
    if (url.protocol !== 'https:' || url.hostname !== 'discord.com'
      || url.username || url.password || url.port || url.search || url.hash) return null;
    return value;
  } catch {
    return null;
  }
}

function logsLink(value) {
  if (typeof value !== 'string' || value.length > 1024 || !/^https?:\/\/[^/\s]\S*$/.test(value)) return null;
  try {
    const url = new URL(value);
    if (!['http:', 'https:'].includes(url.protocol) || !url.hostname || url.username || url.password
      || url.hostname === 'discord.com' || url.pathname.startsWith('/api/webhooks/')) return null;
    return value;
  } catch {
    return null;
  }
}

async function deliver(endpoint, payload) {
  const controller = new AbortController();
  let timer;
  const deadline = new Promise((resolve) => {
    timer = setTimeout(() => { controller.abort(); resolve(); }, DELIVERY_MS);
  });
  const attempt = (async () => {
    try {
      const response = await fetch(endpoint, {
        method: 'POST', redirect: 'error', headers: { 'content-type': 'application/json' },
        body: JSON.stringify(payload), signal: controller.signal,
      });
      // No response text is useful here, for success or failure. Cancellation
      // consumes zero bytes; the deadline bounds even a hung cancellation.
      if (response.body) await response.body.cancel();
    } catch {
      // At-most-once attempt: errors, redirects, 429/5xx and aborts never retry,
      // log exception text, store responses or expose the endpoint/token.
    }
  })();
  await Promise.race([attempt, deadline]);
  clearTimeout(timer);
}

export class ErrorRateAlerts extends DurableObject {
  async ingest(input) {
    const receivedAt = Date.now();
    const batch = canonicalBatch(input);
    if (!batch.observations.length && !batch.incomplete) return;
    try {
      await this.ctx.storage.transaction(async (tx) => {
        const state = await readState(tx);
        // A bounded coverage tombstone outlives capacity refusal and restart.
        // Receipts in it cannot later create a misleading complete prefix.
        if (state.lost !== null && receivedAt < state.lost.end) {
          await saveState(tx, state);
          return;
        }
        let window = state.windows.find((item) => receivedAt >= item.start && receivedAt < item.end);
        if (window === undefined) {
          if (state.windows.length === 2) {
            state.lost = { start: state.lost?.start ?? receivedAt, end: receivedAt + WINDOW_MS };
            await saveState(tx, state);
            return;
          }
          state.lost = null;
          window = { start: receivedAt, end: receivedAt + WINDOW_MS, count: 0, shards: 0, incomplete: false };
          state.windows.push(window);
        }
        const retained = state.windows.reduce((count, item) => count + item.count, 0);
        if (batch.incomplete || window.incomplete || retained + batch.observations.length > OBSERVATION_LIMIT) {
          await discardRows(tx, window);
        } else {
          await appendRows(tx, window, batch.observations);
        }
        await saveState(tx, state);
      });
    } catch {
      // Fixed error only: never reflect an RPC property, storage value or raw
      // exception to the caller. Failed atomic writes must not acknowledge.
      throw new Error('Alert window persistence failed');
    }
  }

  async alarm() {
    const now = Date.now();
    const endpoint = webhookEndpoint(this.env.OPS_ALERT_WEBHOOK_URL);
    const logsUrl = logsLink(this.env.LOGS_URL);
    const payload = await this.ctx.storage.transaction(async (tx) => {
      const state = await readState(tx);
      let reserved = null;
      const pending = [];
      for (const window of state.windows) {
        if (window.end > now) {
          pending.push(window);
          continue;
        }
        if (!window.incomplete) {
          const rows = await readRows(tx, window);
          if (rows !== null) {
            const verdict = evaluateErrorWindow(rows, REDUCER_POLICY);
            if (endpoint !== null && logsUrl !== null && verdict?.verdict === 'breach'
              && now >= state.cooldownUntil && reserved === null) {
              const rendered = renderVerdictAlert(verdict, {
                logsUrl, windowLabel: `DO receive-time [${new Date(window.start).toISOString()}, ${new Date(window.end).toISOString()})`,
              });
              if (rendered !== null) {
                state.cooldownUntil = now + COOLDOWN_MS;
                reserved = rendered;
              }
            }
          }
        }
        if (window.shards) await tx.delete(shardKeys(window));
      }
      state.windows = pending;
      if (state.lost !== null && state.lost.end <= now) state.lost = null;
      await saveState(tx, state);
      return reserved;
    });
    // No transaction is held across external I/O, and no stale snapshot is
    // written afterward. A crash here may lose delivery but cannot retry it.
    if (payload !== null) await deliver(endpoint, payload);
  }
}
