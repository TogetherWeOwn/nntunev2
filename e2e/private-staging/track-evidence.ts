import { randomUUID } from 'node:crypto';

export type RunIdentity = { build_sha: string; run_id: string; run_attempt: string };

const build = /^[a-f0-9]{40}$/;
const decimal = /^[1-9][0-9]{0,15}$/;
const trackId = /^[a-f0-9]{8}-(?:[a-f0-9]{4}-){3}[a-f0-9]{12}$/;
export const trackDigest = /^[a-f0-9]{64}$/;
const binding = /^[A-Za-z0-9_-]{43}$/;

export function privateRunIdentity(buildSha: string, runId = '', runAttempt = ''): RunIdentity {
  if (typeof buildSha !== 'string' || !build.test(buildSha)) throw new Error('Private staging requires a build SHA');
  if (typeof runId !== 'string' || !decimal.test(runId) || typeof runAttempt !== 'string' || !decimal.test(runAttempt)) {
    throw new Error('Private staging requires the Actions run ID and attempt');
  }
  return { build_sha: buildSha, run_id: runId, run_attempt: runAttempt };
}

export function privateTrackObservation(
  run: RunIdentity,
  publication: { trackId: string; sha256: string },
  observedTrackId: unknown,
) {
  if (typeof observedTrackId !== 'string' || !trackId.test(observedTrackId) ||
      observedTrackId !== publication?.trackId) {
    throw new Error('Private staging observed a different track from the fresh publication');
  }
  if (typeof publication.sha256 !== 'string' || !trackDigest.test(publication.sha256)) {
    throw new Error('Private staging requires a publication digest');
  }
  return { track_id: observedTrackId, sha256: publication.sha256,
    ...privateRunIdentity(run?.build_sha, run?.run_id, run?.run_attempt) };
}

export async function privateCutoffObservation<Listener>(
  listener: Listener,
  invitedBinding: string,
  playbackBinding: string,
  readBinding: (listener: Listener) => Promise<string>,
  probe: (listener: Listener) => Promise<ReturnType<typeof privateTrackObservation>>,
) {
  if (typeof invitedBinding !== 'string' || !binding.test(invitedBinding) ||
      typeof playbackBinding !== 'string' || !binding.test(playbackBinding)) {
    throw new Error('Cutoff probes require valid listener session bindings');
  }
  const refusedBinding = await readBinding(listener);
  if (refusedBinding !== playbackBinding) throw new Error('Cutoff probes lost the active playback session');
  const track = await probe(listener);
  if (await readBinding(listener) !== refusedBinding) throw new Error('Cutoff probe session changed');
  // Preserve equality in the report without exporting either session binding.
  const cutoffListener = randomUUID();
  return {
    ...privateTrackObservation(track, { trackId: track?.track_id, sha256: track?.sha256 }, track?.track_id),
    cutoff_listener: cutoffListener,
    refused_listener: invitedBinding === refusedBinding ? cutoffListener : randomUUID(),
  };
}
