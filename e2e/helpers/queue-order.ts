/**
 * Queue-vs-library-selection order verdict for the staging E2E.
 *
 * The offline `apps/web/tests/player-queue.spec.ts` pins queue mechanics
 * with mocked audio; that says nothing about whether the staging player
 * queues a real ingested library in the order the library lists it. This
 * pure helper turns the library's rendered selection order (track ids top to
 * bottom) plus the player queue's observed order into either "matches"
 * (null) or a named failure. Keeping the verdict pure lets
 * e2e/tests/queue-order.spec.ts pin it offline: a missing queue, a short
 * library, or any divergence must never be reported as matching.
 *
 * Ids in messages are synthetic fixture ids (never credentials), so they may
 * be named — unlike URLs, tokens and passwords, which stay out of public CI
 * artifacts.
 */

/**
 * Return null when `queueOrder` holds exactly the `libraryOrder` ids in the
 * same order, otherwise a human-readable failure naming the missing proof.
 * `queueOrder` is null when the page exposes no queue (the queue hooks are
 * absent), which is itself a failure naming the expected hooks.
 */
export function queueOrderFailure(
  libraryOrder: string[],
  queueOrder: string[] | null,
): string | null {
  if (libraryOrder.length === 0) {
    return 'library is empty — ingest spec must run first (queue-order needs ingested tracks)';
  }
  if (libraryOrder.length < 2) {
    return (
      `library holds only 1 track (${libraryOrder[0] ?? '(unknown)'}) — queue-order needs ` +
      '≥2 ingested tracks in library-selection order (the synthetic fixture currently ingests one)'
    );
  }
  if (queueOrder === null) {
    return (
      'player queue is not exposed on /library (expected queue hooks queue-list/queue-item, ' +
      'now-playing and player-next are missing) — the queue UI must be wired to the selected library order'
    );
  }
  if (queueOrder.length === 0) {
    return (
      `player queue is empty but the library lists ${libraryOrder.length} tracks — ` +
      'selecting the library tracks did not fill the queue'
    );
  }
  const seen = new Set<string>();
  for (const id of queueOrder) {
    if (seen.has(id)) {
      return `player queue contains a duplicate id (${id}) — queue ids must be unique`;
    }
    seen.add(id);
  }
  if (queueOrder.length !== libraryOrder.length) {
    const extra = queueOrder.filter((id) => !libraryOrder.includes(id));
    const missing = libraryOrder.filter((id) => !queueOrder.includes(id));
    const detail =
      extra.length > 0
        ? `extra queue id(s): ${extra.join(', ')}`
        : `missing queue id(s): ${missing.join(', ')}`;
    return (
      `player queue length (${queueOrder.length}) does not match library-selection length ` +
      `(${libraryOrder.length}) — ${detail}`
    );
  }
  for (let index = 0; index < libraryOrder.length; index += 1) {
    if (queueOrder[index] !== libraryOrder[index]) {
      return (
        `player queue diverges from library-selection order at position ${index}: ` +
        `expected ${libraryOrder[index]}, observed ${queueOrder[index]}`
      );
    }
  }
  return null;
}
