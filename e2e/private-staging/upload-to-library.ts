import { freshTrialPublication } from '../helpers/private-staging-fixture.js';
import { isQueueTrackId } from '../helpers/queue-order.js';
import { privateRunIdentity, privateTrackObservation, trackDigest, type RunIdentity } from './track-evidence.js';

export async function uploadToLibrary(
  run: RunIdentity,
  digest: string,
  priorIds: readonly string[],
  upload: () => Promise<{ status: number; text: string }>,
  read: (trackId: string) => Promise<{
    status: number;
    items: readonly { id: string }[];
    renderedTrackId: string | null;
  }>,
) {
  const identity = privateRunIdentity(run?.build_sha, run?.run_id, run?.run_attempt);
  if (typeof digest !== 'string' || !trackDigest.test(digest) || !Array.isArray(priorIds) ||
      Array.from(priorIds).some((id) => !isQueueTrackId(id))) {
    throw new Error('Private staging upload requires a digest and prior Library IDs');
  }
  const before = [...priorIds];
  const receipt = await upload();
  if (receipt?.status !== 200 || typeof receipt.text !== 'string' || receipt.text.length > 1024) {
    throw new Error('Private staging upload must succeed with a bounded receipt');
  }
  const publication = freshTrialPublication(receipt.text, digest, before);
  const library = await read(publication.trackId);
  if (library?.status !== 200 || !Array.isArray(library.items) || Array.from(library.items).some((row) =>
    !isQueueTrackId(row?.id)) || library.items.filter(({ id }) => id === publication.trackId).length !== 1) {
    throw new Error('Private staging Library must return the fresh publication exactly once');
  }
  const submitted = privateTrackObservation(identity, publication, publication.trackId);
  const admitted = privateTrackObservation(identity, publication, library.renderedTrackId);

  return {
    publication,
    annotations: [
      { type: 'private-publication', description: JSON.stringify(submitted) },
      { type: 'private-library', description: JSON.stringify(admitted) },
    ],
  };
}
