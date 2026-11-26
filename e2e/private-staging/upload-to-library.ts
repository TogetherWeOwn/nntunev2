import { freshTrialPublication } from '../helpers/private-staging-fixture.js';
import { privateTrackObservation, type RunIdentity } from './track-evidence.js';

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
  const receipt = await upload();
  if (receipt.status !== 200) throw new Error('Private staging upload must succeed');
  const publication = freshTrialPublication(receipt.text, digest, priorIds);
  const library = await read(publication.trackId);
  if (library.status !== 200 || library.items.filter(({ id }) => id === publication.trackId).length !== 1) {
    throw new Error('Private staging Library must return the fresh publication exactly once');
  }
  const submitted = privateTrackObservation(run, publication, publication.trackId);
  const admitted = privateTrackObservation(run, publication, library.renderedTrackId);

  return {
    publication,
    annotations: [
      { type: 'private-publication', description: JSON.stringify(submitted) },
      { type: 'private-library', description: JSON.stringify(admitted) },
    ],
  };
}
