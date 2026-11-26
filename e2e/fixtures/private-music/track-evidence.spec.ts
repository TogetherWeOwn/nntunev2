import { expect, test } from '@playwright/test';
import { freshTrialPublication, privateStagingAudio } from '../../helpers/private-staging-fixture.js';
import { privateRunIdentity, privateTrackObservation } from '../../private-staging/track-evidence.js';
import { uploadToLibrary } from '../../private-staging/upload-to-library.js';

test('private track evidence requires the exact newly published track ID', () => {
  const { sha256 } = privateStagingAudio().find(({ file }) => file === 'tone-a.flac')!;
  const publishedTrackId = '00000000-0000-4000-8000-000000000001';
  const existingTrackId = '00000000-0000-4000-8000-000000000002';
  const publication = freshTrialPublication(
    JSON.stringify({ trackId: publishedTrackId, sha256 }), sha256, [existingTrackId],
  );
  const buildSha = 'a'.repeat(40);
  const run = privateRunIdentity(buildSha, '123', '2');

  expect(privateTrackObservation(run, publication, publishedTrackId)).toEqual({
    track_id: publishedTrackId,
    sha256,
    build_sha: buildSha,
    run_id: '123',
    run_attempt: '2',
  });
  expect(() => privateTrackObservation(run, publication, existingTrackId))
    .toThrow('Private staging observed a different track from the fresh publication');
});

test('upload-to-Library evidence selects the fresh publication among same-title tracks', async () => {
  const { sha256 } = privateStagingAudio().find(({ file }) => file === 'tone-a.flac')!;
  const freshRow = { id: '00000000-0000-4000-8000-000000000001', title: 'Synthetic Tone' };
  const companionRow = { ...freshRow, id: '00000000-0000-4000-8000-000000000002' };
  const run = privateRunIdentity('a'.repeat(40), '123', '2');
  const before = { status: 200, items: [companionRow] };
  const after = { status: 200, items: [...before.items, freshRow] };
  const exercise = (library: typeof after, renderedTrackId = freshRow.id) => uploadToLibrary(
    run, sha256, before.items.map(({ id }) => id),
    async () => ({ status: 200, text: JSON.stringify({ trackId: freshRow.id, sha256 }) }),
    async (trackId) => {
      expect(trackId).toBe(freshRow.id);
      return { ...library, renderedTrackId };
    },
  );

  const result = await exercise(after);
  expect(result.publication).toEqual({ trackId: freshRow.id, sha256 });
  const observation = { track_id: freshRow.id, sha256, build_sha: 'a'.repeat(40), run_id: '123', run_attempt: '2' };
  expect(result.annotations.map(({ type, description }) => ({ type, observation: JSON.parse(description) }))).toEqual([
    { type: 'private-publication', observation },
    { type: 'private-library', observation },
  ]);
  for (const rejected of [before, { ...after, items: [...after.items, freshRow] }, { ...after, status: 201 }]) {
    await expect(exercise(rejected)).rejects.toThrow('Private staging Library must return the fresh publication exactly once');
  }
  await expect(exercise(after, companionRow.id)).rejects.toThrow(
    'Private staging observed a different track from the fresh publication',
  );
});
