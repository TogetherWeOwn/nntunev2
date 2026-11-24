// Synthetic handles exercise ownership, not the runtime's network or TLS implementation.
export function socketFixture(acknowledged) {
  const calls = { upgrades: 0, originalCloses: 0, replacementCloses: 0, observedRejections: 0 };
  const rejectedClose = () => {
    const promise = Promise.reject(new Error("test rejection"));
    const catchRejection = promise.catch;
    promise.catch = function (handler) {
      calls.observedRejections++;
      return catchRejection.call(this, handler);
    };
    return promise;
  };
  const socket = (close) => ({
    readable: new ReadableStream(),
    writable: new WritableStream(),
    close,
  });
  const replacement = socket(() => {
    calls.replacementCloses++;
    return acknowledged ? Promise.resolve() : rejectedClose();
  });
  if (acknowledged) {
    calls.closedAcknowledgements = 0;
    Object.defineProperty(replacement, "closed", {
      get: () => new Promise((resolve) => setTimeout(() => {
        calls.closedAcknowledgements++;
        resolve();
      }, 0)),
    });
  }
  const original = socket(() => { calls.originalCloses++; return rejectedClose(); });
  original.startTls = () => { calls.upgrades++; return replacement; };
  return { original, replacement, calls };
}

export function stalledStartupFixture(stallOpen) {
  const calls = { reads: 0, writes: 0, closes: 0 };
  let finishOpen;
  const opened = new Promise((resolve) => { finishOpen = () => resolve({}); });
  let reader;
  const readable = new ReadableStream({
    start(controller) { reader = controller; },
    pull() {
      calls.reads++;
      return new Promise(() => {}); // Accept startup, but never answer it.
    },
  }, { highWaterMark: 0 });
  const writable = new WritableStream({ write() { calls.writes++; } });
  if (!stallOpen) setTimeout(finishOpen, 1000);
  return [{
    opened, readable, writable,
    close() {
      if (++calls.closes === 1) {
        finishOpen(); // Late readiness must not start a cancelled handshake.
        reader.close();
      }
      return Promise.resolve();
    },
    closed: Promise.resolve(),
  }, calls];
}
