import { expect, test } from '@playwright/test';
import { freshTrialPublication, privateStagingAudio } from '../../helpers/private-staging-fixture.js';
import { privateLibraryObservation, privateRunIdentity, privateTrackObservation } from '../../private-staging/track-evidence.js';

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

test('upload-to-Library evidence selects the fresh publication among same-title tracks', () => {
  const { sha256 } = privateStagingAudio().find(({ file }) => file === 'tone-a.flac')!;
  const freshRow = { id: '00000000-0000-4000-8000-000000000001', title: 'Synthetic Tone' };
  const companionRow = { ...freshRow, id: '00000000-0000-4000-8000-000000000002' };
  const before = { status: 200, items: [companionRow] };
  const publication = freshTrialPublication(
    JSON.stringify({ trackId: freshRow.id, sha256 }), sha256, before.items.map(({ id }) => id),
  );
  const run = privateRunIdentity('a'.repeat(40), '123', '2');
  const after = { status: 200, items: [...before.items, freshRow] };

  expect(privateLibraryObservation(run, publication, after, freshRow.id)).toEqual({
    track_id: freshRow.id,
    sha256,
    ...run,
  });
  for (const rejected of [before, { ...after, items: [...after.items, freshRow] }, { ...after, status: 403 }]) {
    expect(() => privateLibraryObservation(run, publication, rejected, freshRow.id))
      .toThrow('Private staging Library must return the fresh publication exactly once');
  }
  expect(() => privateLibraryObservation(run, publication, after, companionRow.id))
    .toThrow('Private staging observed a different track from the fresh publication');
});
