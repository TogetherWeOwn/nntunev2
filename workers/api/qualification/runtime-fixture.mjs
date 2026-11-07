import assert from 'node:assert/strict';
import { readFile, readdir } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { database } from './database.mjs';

export const root = fileURLToPath(new URL('../../', import.meta.url));

export async function generatedModules(prefix, folder = 'build') {
  const modules = [];
  async function visit(relative) {
    for (const item of await readdir(path.join(root, relative), { withFileTypes: true })) {
      const name = `${relative}/${item.name}`;
      if (item.isDirectory()) await visit(name);
      else if (/\.(mjs|js|wasm)$/.test(name)) {
        modules.push({
          type: name.endsWith('.wasm') ? 'CompiledWasm' : 'ESModule',
          path: name,
          contents: await readFile(path.join(root, name)),
        });
      }
    }
  }
  await visit(`${prefix}${folder}`);
  assert.ok(modules.some(m => m.type === 'CompiledWasm'), 'actual Wasm artifact required');
  return modules;
}

export async function sourceModule(relative) {
  return { type: 'ESModule', path: relative, contents: await readFile(path.join(root, relative), 'utf8') };
}

export function worker(name, modules, options = {}) {
  return {
    name, modules, modulesRoot: root,
    compatibilityDate: '2026-10-23',
    // An accidental external fetch must fail, never contact a real service.
    outboundService: () => new Response(null, { status: 502 }),
    ...options,
  };
}

// The ordinary suite and crash subprocess must use exactly the same storage,
// private service, native Queue and generated Rust artifacts for this fixture.
export async function flacWorkers(configured, connection = database) {
  return [
    worker('api-database', [
      await sourceModule('api/src/runtime.mjs'), ...await generatedModules('api/'),
    ], { bindings: configured, hyperdrives: { HYPERDRIVE: connection } }),
    worker('composite-flac', [
      await sourceModule('api/qualification/composite-probe.mjs'), ...await generatedModules('ingest/'),
    ], {
      bindings: { GENERATION_PREPARATION_MODE: 'preparation_only', HYPERDRIVE_CACHING_DISABLED: '1' },
      hyperdrives: { LIBRARY_DB: connection },
      serviceBindings: { API_PREPARATION_PLANS: { name: 'api-database', entrypoint: 'IngestPreparationPlans' } },
      r2Buckets: { INGEST_SCRATCH: 'qualification-flac' },
      queueProducers: { INGEST_QUEUE: 'qualification-flac-queue' },
      queueConsumers: { 'qualification-flac-queue': {
        maxBatchSize: 1, maxBatchTimeout: 0, maxRetries: 0,
        deadLetterQueue: 'qualification-flac-dead-letter',
      } },
    }),
    worker('flac-dead-letter', [await sourceModule('api/qualification/dead-letter-probe.mjs')], {
      r2Buckets: { INGEST_SCRATCH: 'qualification-flac' },
      queueConsumers: { 'qualification-flac-dead-letter': { maxBatchSize: 1, maxBatchTimeout: 0 } },
    }),
  ];
}
