// Drop cannot await close. Observe rejection without retaining a Rust callback.
export function observeSocketClose(socket) {
  try {
    Promise.resolve(socket.close()).catch(() => {});
  } catch (_) {
    // Closing is best effort, including synchronous runtime failures.
  }
}
