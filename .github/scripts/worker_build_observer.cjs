// Observation-only preload for the exact Wrangler build boundary. Never use for deploys.
'use strict';
const fs = require('node:fs');
const crypto = require('node:crypto');
const Module = require('node:module');
const path = require('node:path');

const expectedHash = '18dbf242b542e171c993d68cc41a4e58bc4f533392fc91b71e29ffeee7bf6c0c';
const anchor = `  const sourceMaps = uploadSourceMaps === false ? void 0 : loadSourceMaps(main2, modules, bundle, {
    strict: uploadSourceMaps === true
  });
  return {
    modules,
    sourceMaps,
    dependencies,
    resolvedEntryPointPath,
    bundleType,
    content
  };`;
const types = Object.freeze({
  esm: 'application/javascript+module', commonjs: 'application/javascript',
  'compiled-wasm': 'application/wasm', buffer: 'application/octet-stream',
  text: 'text/plain', python: 'text/x-python',
  'python-requirement': 'text/x-python-requirement',
});
const digest = data => crypto.createHash('sha256').update(data).digest('hex');
const target = fs.realpathSync(process.env.WORKER_ORACLE_CLI);
const output = process.env.WORKER_ORACLE_OUTPUT;
if (!output || !path.isAbsolute(output) || path.basename(target) !== 'cli.js' ||
    path.basename(path.dirname(target)) !== 'wrangler-dist') throw Error('invalid oracle paths');
let verified = false;
let observed = false;
process.on('exit', () => {
  if (!observed) {
    process.stderr.write('Wrangler build observation missing\n');
    process.exitCode = 1;
  }
});
globalThis.__workerBuildReceipt = (main, modules, maps) => {
  if (!verified || observed || !Array.isArray(modules) || (maps !== undefined && !Array.isArray(maps))) {
    throw Error('invalid or repeated build observation');
  }
  const tuple = (item, map = false) => {
    if (!item || typeof item.name !== 'string' || !item.name ||
        !(typeof item.content === 'string' || Buffer.isBuffer(item.content) || item.content instanceof Uint8Array)) {
      throw Error('invalid build tuple');
    }
    if (!map && (typeof item.type !== 'string' || !Object.hasOwn(types, item.type))) {
      throw Error('unknown build module type');
    }
    const mime = map ? 'application/source-map' : types[item.type];
    const bytes = Buffer.from(item.content);
    return { name: item.name, type: mime, bytes: bytes.length, sha256: digest(bytes) };
  };
  const entry = tuple(main);
  const receipt = { entry, modules: modules.map(m => tuple(m)), maps: (maps || []).map(m => tuple(m, true)) };
  const names = [entry, ...receipt.modules, ...receipt.maps].map(item => item.name);
  if (new Set(names).size !== names.length) throw Error('duplicate build names');
  fs.writeFileSync(output, JSON.stringify(receipt) + '\n', { flag: 'wx', mode: 0o600 });
  observed = true;
  process.exit(0);
};
const original = Module._extensions['.js'];
Module._extensions['.js'] = function (module, filename) {
  if (fs.realpathSync(filename) !== target) return original(module, filename);
  const source = fs.readFileSync(filename, 'utf8');
  if (digest(Buffer.from(source)) !== expectedHash) throw Error('Wrangler CLI hash mismatch');
  const first = source.indexOf(anchor);
  if (first < 0 || source.indexOf(anchor, first + 1) !== -1) throw Error('Wrangler build anchor mismatch');
  const injected = anchor.replace('  return {', '  globalThis.__workerBuildReceipt(main2, modules, sourceMaps);\n  return {');
  verified = true;
  module._compile(source.slice(0, first) + injected + source.slice(first + anchor.length), filename);
};
