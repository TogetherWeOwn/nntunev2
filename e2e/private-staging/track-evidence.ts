import { randomUUID } from 'node:crypto';

type RunIdentity = { build_sha: string; run_id: string; run_attempt: string };

export function privateRunIdentity(buildSha: string, runId = '', runAttempt = ''): RunIdentity {
  const decimal = /^[1-9][0-9]{0,15}$/;
  if (!decimal.test(runId) || !decimal.test(runAttempt)) {
    throw new Error('Private staging requires the Actions run ID and attempt');
  }
  return { build_sha: buildSha, run_id: runId, run_attempt: runAttempt };
}

export function privateTrackObservation(
  run: RunIdentity,
  publication: { trackId: string; sha256: string },
  observedTrackId: unknown,
) {
  if (observedTrackId !== publication.trackId) {
    throw new Error('Private staging observed a different track from the fresh publication');
  }
  return { track_id: observedTrackId, sha256: publication.sha256, ...run };
}

export async function privateCutoffObservation<Listener>(
  listener: Listener,
  invitedBinding: string,
  playbackBinding: string,
  readBinding: (listener: Listener) => Promise<string>,
  probe: (listener: Listener) => Promise<ReturnType<typeof privateTrackObservation>>,
) {
  const refusedBinding = await readBinding(listener);
  if (refusedBinding !== playbackBinding) throw new Error('Cutoff probes lost the active playback session');
  const track = await probe(listener);
  if (await readBinding(listener) !== refusedBinding) throw new Error('Cutoff probe session changed');
  // Preserve equality in the report without exporting either session binding.
  const cutoffListener = randomUUID();
  return {
    ...track,
    cutoff_listener: cutoffListener,
    refused_listener: invitedBinding === refusedBinding ? cutoffListener : randomUUID(),
  };
}
