// Privacy-safe projection of one Cloudflare Tail invocation into an
// error-rate observation. Pure, deterministic and side-effect-free:
// no imports, network, environment reads, logging, clocks, randomness,
// timers or storage. Input is one JSON-compatible Tail invocation.
//
// Frozen v1 schema (http_status omitted when unknown):
// {schema_version: 1, service, environment, outcome,
//  http_status?: 100..599,
//  failure_flags: {http_5xx, exception, exceeded_cpu, exceeded_memory},
//  build_sha, release, route, error_class}
// Invalid policy, malformed root event or nonmatching producer => null.
//
// Source status is event.response.status only. Identity/grouping comes only
// from JSON strings in logs[].message[] containing the API's exact six-key
// served envelope (ts, level, worker, trace_id, event, fields) with
// worker='api' and event='api.request.served'. Bounds: <=64 logs,
// <=4 messages/log, <=16384 UTF-8 bytes/string before parsing. Any
// truncated/oversized scan or malformed logs/message container makes all
// identity/grouping unknown while platform outcome/status/exceptions stay
// usable. This module is not wired or deployed; it performs no delivery.

const SCHEMA_VERSION = 1;
const MAX_LOGS = 64;
const MAX_MESSAGES = 4;
const MAX_STRING_BYTES = 16384;
const MAX_ROUTE_CHARS = 128;
const MAX_ALLOWLIST = 64;

const EXPECTED_SCRIPT = 'nntune-api-staging';
const EXPECTED_SERVICE = 'api';
const EXPECTED_ENVIRONMENT = 'staging';

const SHA40_RE = /^[0-9a-f]{40}$/;
const TRACE32_RE = /^[0-9a-f]{32}$/;
const ERROR_CLASS_RE = /^[a-z][a-z0-9_]{0,63}$/;
const METHOD_RE = /^[A-Z]+$/;
const PATH_RE = /^[A-Za-z0-9\-._~!$&'()*+,;=:@%/{}]+$/;

function isValidErrorClass(value) {
  return typeof value === 'string' && ERROR_CLASS_RE.test(value);
}

function isValidRoute(value) {
  if (typeof value !== 'string') return false;
  if (value.length === 0 || value.length > MAX_ROUTE_CHARS) return false;
  for (let i = 0; i < value.length; i += 1) {
    const code = value.charCodeAt(i);
    if (code <= 0x1f || code === 0x7f) return false;
  }
  const firstSpace = value.indexOf(' ');
  if (firstSpace <= 0) return false;
  if (value.indexOf(' ', firstSpace + 1) !== -1) return false;
  if (firstSpace === value.length - 1) return false;
  const method = value.slice(0, firstSpace);
  const path = value.slice(firstSpace + 1);
  if (!METHOD_RE.test(method)) return false;
  if (path[0] !== '/') return false;
  if (path.indexOf('?') !== -1 || path.indexOf('#') !== -1 || path.indexOf('\\') !== -1) return false;
  if (!PATH_RE.test(path)) return false;
  return true;
}

function isValidPolicy(policy) {
  if (typeof policy !== 'object' || policy === null || Array.isArray(policy)) return false;
  if (policy.script_name !== EXPECTED_SCRIPT) return false;
  if (policy.service !== EXPECTED_SERVICE) return false;
  if (policy.environment !== EXPECTED_ENVIRONMENT) return false;
  const routes = policy.routes;
  const errorClasses = policy.error_classes;
  if (!Array.isArray(routes) || !Array.isArray(errorClasses)) return false;
  if (routes.length > MAX_ALLOWLIST || errorClasses.length > MAX_ALLOWLIST) return false;
  for (let i = 0; i < routes.length; i += 1) {
    if (!isValidRoute(routes[i])) return false;
  }
  for (let i = 0; i < errorClasses.length; i += 1) {
    if (!isValidErrorClass(errorClasses[i])) return false;
  }
  return true;
}

function normalizeOutcome(raw) {
  if (raw === 'ok') return 'ok';
  if (raw === 'exception') return 'exception';
  if (raw === 'exceededCpu') return 'exceeded_cpu';
  if (raw === 'exceededMemory') return 'exceeded_memory';
  return 'unknown';
}

function isValidTs(ts) {
  if (typeof ts !== 'string') return false;
  if (!ts.endsWith('Z')) return false;
  try {
    const parsed = Date.parse(ts);
    if (Number.isNaN(parsed)) return false;
    return true;
  } catch {
    return false;
  }
}

function isValidServedEnvelope(obj) {
  let keys;
  try {
    keys = Object.keys(obj);
  } catch {
    return false;
  }
  if (keys.length !== 6) return false;
  if (
    keys.indexOf('ts') === -1
    || keys.indexOf('level') === -1
    || keys.indexOf('worker') === -1
    || keys.indexOf('trace_id') === -1
    || keys.indexOf('event') === -1
    || keys.indexOf('fields') === -1
  ) {
    return false;
  }
  let ts;
  let level;
  let traceId;
  let fields;
  try {
    ts = obj.ts;
    level = obj.level;
    traceId = obj.trace_id;
    fields = obj.fields;
  } catch {
    return false;
  }
  if (!isValidTs(ts)) return false;
  if (level !== 'debug' && level !== 'info' && level !== 'warn' && level !== 'error') return false;
  if (typeof traceId !== 'string' || !TRACE32_RE.test(traceId)) return false;
  if (typeof fields !== 'object' || fields === null || Array.isArray(fields)) return false;
  let fieldKeys;
  try {
    fieldKeys = Object.keys(fields);
  } catch {
    return false;
  }
  for (let i = 0; i < fieldKeys.length; i += 1) {
    let value;
    try {
      value = fields[fieldKeys[i]];
    } catch {
      return false;
    }
    if (value === null) continue;
    const kind = typeof value;
    if (kind === 'string' || kind === 'number' || kind === 'boolean') continue;
    return false;
  }
  return true;
}

export function projectTailEvent(event, policy) {
  try {
    if (!isValidPolicy(policy)) return null;
    if (typeof event !== 'object' || event === null || Array.isArray(event)) return null;
    let scriptName;
    try {
      scriptName = event.scriptName;
    } catch {
      return null;
    }
    if (scriptName !== policy.script_name) return null;

    let rawOutcome;
    try {
      rawOutcome = event.outcome;
    } catch {
      rawOutcome = undefined;
    }
    const outcome = normalizeOutcome(rawOutcome);

    let exceptions;
    try {
      exceptions = event.exceptions;
    } catch {
      exceptions = undefined;
    }
    const hasExceptionsArray = Array.isArray(exceptions) && exceptions.length > 0;
    const exceptionFlag = rawOutcome === 'exception' || hasExceptionsArray;
    const exceededCpuFlag = rawOutcome === 'exceededCpu';
    const exceededMemoryFlag = rawOutcome === 'exceededMemory';

    let httpStatus;
    let hasHttpStatus = false;
    try {
      const inner = event.event;
      if (typeof inner === 'object' && inner !== null && !Array.isArray(inner)) {
        const response = inner.response;
        if (typeof response === 'object' && response !== null && !Array.isArray(response)) {
          const status = response.status;
          if (typeof status === 'number' && Number.isInteger(status) && status >= 100 && status <= 599) {
            httpStatus = status;
            hasHttpStatus = true;
          }
        }
      }
    } catch {
      hasHttpStatus = false;
    }
    const http5xxFlag = hasHttpStatus && httpStatus >= 500 && httpStatus <= 599;

    let logs;
    try {
      logs = event.logs;
    } catch {
      logs = undefined;
    }

    let incomplete = false;
    let identityPoisoned = false;
    let routePoisoned = false;
    let errorPoisoned = false;
    let observedSha = null;
    let observedRoute = null;
    let observedError = null;

    if (logs !== undefined) {
      if (!Array.isArray(logs)) {
        incomplete = true;
      } else {
        if (logs.length > MAX_LOGS) incomplete = true;
        const inspectLogs = logs.length > MAX_LOGS ? MAX_LOGS : logs.length;
        let encoder = null;
        try {
          encoder = new TextEncoder();
        } catch {
          encoder = null;
        }
        if (encoder === null) {
          incomplete = true;
        } else {
          for (let i = 0; i < inspectLogs; i += 1) {
            let entry;
            try {
              entry = logs[i];
            } catch {
              incomplete = true;
              continue;
            }
            if (typeof entry !== 'object' || entry === null || Array.isArray(entry)) {
              incomplete = true;
              continue;
            }
            let messages;
            try {
              messages = entry.message;
            } catch {
              incomplete = true;
              continue;
            }
            if (!Array.isArray(messages)) {
              incomplete = true;
              continue;
            }
            if (messages.length > MAX_MESSAGES) incomplete = true;
            const inspectMessages = messages.length > MAX_MESSAGES ? MAX_MESSAGES : messages.length;
            for (let j = 0; j < inspectMessages; j += 1) {
              let text;
              try {
                text = messages[j];
              } catch {
                incomplete = true;
                continue;
              }
              if (typeof text !== 'string') continue;
              let byteLength;
              try {
                byteLength = encoder.encode(text).length;
              } catch {
                incomplete = true;
                continue;
              }
              if (byteLength > MAX_STRING_BYTES) {
                incomplete = true;
                continue;
              }
              let parsed;
              try {
                parsed = JSON.parse(text);
              } catch {
                continue;
              }
              if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) continue;
              let worker;
              let name;
              try {
                worker = parsed.worker;
                name = parsed.event;
              } catch {
                continue;
              }
              if (worker !== EXPECTED_SERVICE || name !== 'api.request.served') continue;
              if (!isValidServedEnvelope(parsed)) {
                identityPoisoned = true;
                routePoisoned = true;
                errorPoisoned = true;
                continue;
              }
              let fields;
              try {
                fields = parsed.fields;
              } catch {
                identityPoisoned = true;
                routePoisoned = true;
                errorPoisoned = true;
                continue;
              }
              let buildSha;
              let release;
              try {
                buildSha = fields.build_sha;
                release = fields.release;
              } catch {
                identityPoisoned = true;
                buildSha = undefined;
                release = undefined;
              }
              if (
                typeof buildSha !== 'string'
                || typeof release !== 'string'
                || !SHA40_RE.test(buildSha)
                || !SHA40_RE.test(release)
                || buildSha !== release
              ) {
                identityPoisoned = true;
              } else if (observedSha === null) {
                observedSha = buildSha;
              } else if (observedSha !== buildSha) {
                identityPoisoned = true;
              }
              let routeValue;
              try {
                routeValue = fields.route;
              } catch {
                routeValue = undefined;
              }
              if (typeof routeValue !== 'string' || policy.routes.indexOf(routeValue) === -1) {
                routePoisoned = true;
              } else if (observedRoute === null) {
                observedRoute = routeValue;
              } else if (observedRoute !== routeValue) {
                routePoisoned = true;
              }
              let errorValue;
              try {
                errorValue = fields.error_class;
              } catch {
                errorValue = undefined;
              }
              if (typeof errorValue !== 'string' || policy.error_classes.indexOf(errorValue) === -1) {
                errorPoisoned = true;
              } else if (observedError === null) {
                observedError = errorValue;
              } else if (observedError !== errorValue) {
                errorPoisoned = true;
              }
            }
          }
        }
      }
    }

    let buildShaOut = 'unknown';
    let releaseOut = 'unknown';
    if (!incomplete && !identityPoisoned && observedSha !== null) {
      buildShaOut = observedSha;
      releaseOut = observedSha;
    }
    let routeOut = 'unknown';
    if (!incomplete && !routePoisoned && observedRoute !== null) {
      routeOut = observedRoute;
    }
    let errorOut = 'unknown';
    if (!incomplete && !errorPoisoned && observedError !== null) {
      errorOut = observedError;
    }

    return {
      schema_version: SCHEMA_VERSION,
      service: policy.service,
      environment: policy.environment,
      outcome,
      ...(hasHttpStatus ? { http_status: httpStatus } : {}),
      failure_flags: {
        http_5xx: http5xxFlag,
        exception: exceptionFlag,
        exceeded_cpu: exceededCpuFlag,
        exceeded_memory: exceededMemoryFlag,
      },
      build_sha: buildShaOut,
      release: releaseOut,
      route: routeOut,
      error_class: errorOut,
    };
  } catch {
    return null;
  }
}
