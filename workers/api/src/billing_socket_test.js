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
  if (acknowledged) replacement.closed = Promise.resolve();
  const original = socket(() => { calls.originalCloses++; return rejectedClose(); });
  original.startTls = () => { calls.upgrades++; return replacement; };
  return { original, replacement, calls };
}
