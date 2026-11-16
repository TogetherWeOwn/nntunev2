import { createHash } from "node:crypto";
import { cp, mkdir, mkdtemp, readFile, readdir, realpath, rm, writeFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "../../../..");
const hash = (data) => createHash("sha256").update(data).digest("hex");
const deadline = async (label, ms, operation) => {
  let timer;
  try {
    return await Promise.race([
      Promise.resolve().then(operation),
      new Promise((_, reject) => { timer = setTimeout(() => reject(new Error(`${label} timeout`)), ms); }),
    ]);
  } finally { clearTimeout(timer); }
};

async function hashes(dir, base = dir) {
  const result = {};
  for (const entry of await readdir(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) Object.assign(result, await hashes(path, base));
    else if (entry.isFile()) result[relative(base, path)] = hash(await readFile(path));
    else throw new Error(`unsupported bundle entry: ${relative(base, path)}`);
  }
  return result;
}

async function realRuntime(nodeRoot) {
  if (process.env.WORKERD_BIN || process.env.MINIFLARE_WORKERD_PATH) throw new Error("runtime override forbidden");
  const req = createRequire(join(resolve(nodeRoot), "collector-resolve.cjs"));
  const miniflarePath = req.resolve("miniflare");
  const mfRequire = createRequire(miniflarePath);
  const mfWorkerd = mfRequire.resolve("workerd");
  const selectedWorkerd = req.resolve("workerd");
  if (await realpath(mfWorkerd) !== await realpath(selectedWorkerd)) throw new Error("workerd resolution mismatch");
  const version = (r, name) => r(`${name}/package.json`).version;
  const binary = await realpath(req("workerd").default);
  return {
    Miniflare: (await import(pathToFileURL(miniflarePath).href)).Miniflare,
    versions: { miniflare: version(req, "miniflare"), workerd: version(req, "workerd") },
    runtimeBinary: { path: binary, sha256: hash(await readFile(binary)) },
  };
}

export async function collectRuntimeCases(
  { bundle, entry, sourceSha, nodeRoot, out, compatibilityDate, cases },
  { loadRuntime = realRuntime } = {},
) {
  const evidence = { sourceSha, generatedHashes: {}, probeEntrySha256: null, pins: null, versions: null,
    runtimeBinary: null, compatibilityDate, cases: {}, runtimeStdio: "", errors: [], cleanup: { disposed: false, removed: false } };
  let work, runtime;
  const error = (phase, e) => evidence.errors.push(`${phase}: ${e?.message || String(e)}`);
  const append = (kind, chunk) => {
    const remaining = 65536 - Buffer.byteLength(evidence.runtimeStdio);
    if (remaining > 0) {
      const data = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
      let text = Buffer.concat([Buffer.from(`[${kind}] `), data.subarray(0, remaining)]).subarray(0, remaining).toString("utf8");
      while (Buffer.byteLength(text) > remaining) text = text.slice(0, -1);
      evidence.runtimeStdio += text;
    }
  };
  try {
    if (!/^[0-9a-f]{40}$/.test(sourceSha || "") || !bundle || !entry || !nodeRoot || !out || !compatibilityDate ||
        !Array.isArray(cases) || !cases.length || new Set(cases).size !== cases.length ||
        cases.some((name) => typeof name !== "string" || !/^[a-z][a-z0-9-]*$/.test(name))) throw new Error("invalid collector arguments");
    evidence.pins = JSON.parse(await readFile(join(root, ".github/worker-toolchain.json"), "utf8")).runtime;
    const loaded = await deadline("runtime resolution", 15000, () => loadRuntime(nodeRoot));
    evidence.versions = loaded.versions;
    evidence.runtimeBinary = loaded.runtimeBinary;
    if (["miniflare", "workerd"].some((name) => !evidence.pins[name] || evidence.pins[name] !== loaded.versions?.[name])) throw new Error("runtime pin mismatch");
    if (!loaded.runtimeBinary?.path || !/^[0-9a-f]{64}$/.test(loaded.runtimeBinary.sha256 || "")) throw new Error("runtime binary identity missing");
    const sourceHashes = await hashes(resolve(bundle));
    if (!sourceHashes["worker/shim.mjs"] || !Object.keys(sourceHashes).some((name) => name.endsWith(".wasm"))) throw new Error("generated bundle incomplete");
    work = await mkdtemp(join(tmpdir(), "repair-collector-"));
    await cp(resolve(bundle), work, { recursive: true, dereference: false });
    evidence.generatedHashes = await hashes(work);
    if (JSON.stringify(Object.entries(sourceHashes).sort()) !== JSON.stringify(Object.entries(evidence.generatedHashes).sort())) throw new Error("bundle copy mismatch");
    const entryBytes = await readFile(entry);
    evidence.probeEntrySha256 = hash(entryBytes);
    if (evidence.generatedHashes["worker/probe_entry.mjs"]) throw new Error("probe entry conflicts with bundle");
    await writeFile(join(work, "worker/probe_entry.mjs"), entryBytes, { flag: "wx" });
    runtime = new loaded.Miniflare({ modules: true, scriptPath: join(work, "worker/probe_entry.mjs"), modulesRoot: work,
      compatibilityDate, handleRuntimeStdio(stdout, stderr) {
        stdout.on("data", (chunk) => append("stdout", chunk));
        stderr.on("data", (chunk) => append("stderr", chunk));
      }, verbose: false });
    await deadline("startup", 15000, () => runtime.ready);
    for (const name of cases) {
      try {
        const result = await deadline(`case ${name}`, 15000, async () => {
          const response = await runtime.dispatchFetch("http://probe.invalid/", { headers: { "x-probe-case": name }, signal: AbortSignal.timeout(15000) });
          const chunks = [];
          let size = 0;
          for await (const chunk of response.body) {
            size += chunk.length;
            if (size > 1024 * 1024) throw new Error("case output exceeds 1 MiB");
            chunks.push(chunk);
          }
          const value = JSON.parse(Buffer.concat(chunks).toString("utf8"));
          if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("case result must be an object");
          return { ...value, httpStatus: response.status };
        });
        evidence.cases[name] = result;
      } catch (e) { error(`case ${name}`, e); break; }
    }
  } catch (e) { error("collection", e); }
  finally {
    if (runtime) {
      try { await deadline("disposal", 10000, () => runtime.dispose()); evidence.cleanup.disposed = true; }
      catch (e) { error("disposal", e); }
    }
    if (work) {
      try { await rm(work, { recursive: true, force: true }); evidence.cleanup.removed = true; }
      catch (e) { error("cleanup", e); }
    }
    try {
      await mkdir(out, { recursive: true });
      await writeFile(join(out, "runtime-stdio.log"), evidence.runtimeStdio);
      await writeFile(join(out, "evidence.json"), JSON.stringify(evidence, null, 2));
    } catch (e) { error("evidence write", e); }
  }
  return evidence;
}
