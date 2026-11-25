import { expect, test } from '@playwright/test';
import { freshTrialPublication, privateStagingAudio } from '../../helpers/private-staging-fixture.js';
import { privateRunIdentity, privateTrackObservation } from '../../private-staging/track-evidence.js';

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
