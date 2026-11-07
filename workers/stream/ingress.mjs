// This runs before the generated SDK fetch converts the URL into Rust.
// Platform Request creation and its JS url string are outside this bound.
const MAX_URL_BYTES = 256;
const guarded = new WeakSet();

function admitsUrl(request) {
  let url;
  try {
    url = request.url;
  } catch {
    return false;
  }
  if (typeof url !== "string" || url.length > MAX_URL_BYTES) return false;

  // Count UTF-8 without allocating an encoded copy. Lone surrogates count as
  // the three-byte replacement character, like Web IDL / TextEncoder.
  let bytes = 0;
  for (let i = 0; i < url.length; i++) {
    const code = url.charCodeAt(i);
    if (code <= 0x7f) bytes++;
    else if (code <= 0x7ff) bytes += 2;
    else if (code >= 0xd800 && code <= 0xdbff && i + 1 < url.length
      && url.charCodeAt(i + 1) >= 0xdc00 && url.charCodeAt(i + 1) <= 0xdfff) {
      bytes += 4;
      i++;
    } else bytes += 3;
    if (bytes > MAX_URL_BYTES) return false;
  }
  return true;
}

export function guardDurableObjects(exports) {
  // Validate the entire export contract before changing either class. No
  // unguarded fallback if a future SDK changes its descriptors or proxies.
  const methods = ["NntpAccount", "NntpPool"].map((name) => {
    const ctor = exports[name];
    const prototype = typeof ctor === "function" ? ctor.prototype : undefined;
    const descriptor = prototype && Object.getOwnPropertyDescriptor(prototype, "fetch");
    if (!descriptor || typeof descriptor.value !== "function"
      || (!descriptor.configurable && !descriptor.writable)) {
      throw new Error("unsupported Durable Object fetch export");
    }
    return { prototype, descriptor };
  });
  if (methods[0].prototype === methods[1].prototype) {
    throw new Error("unsupported Durable Object class identity");
  }
  for (const { prototype, descriptor } of methods) {
    const original = descriptor.value;
    if (guarded.has(original)) continue;
    const fetch = function (request) {
      if (!admitsUrl(request)) {
        return new Response("not found", { status: 404 });
      }
      return Reflect.apply(original, this, arguments);
    };
    guarded.add(fetch);
    Object.defineProperty(prototype, "fetch", { ...descriptor, value: fetch });
  }
}
