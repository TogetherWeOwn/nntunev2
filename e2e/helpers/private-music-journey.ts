import { API, TESTIDS } from './contract.js';
import { playbackFailure, type PlaybackEvidence } from './playback.js';
import { resumeFloorAfterSeek, seekProofFailure, type TimedPlayer } from './player-ui.js';
import { queueOrderFailure } from './queue-order.js';

// Existing hooks only. The owner upload and publish routes will supply their
// own selectors when mounted.
export const PRIVATE_MUSIC = {
  issueInvite: {
    path: '/admin/invites/issue',
    single: 'Single invite',
    email: 'Email (optional)',
    submit: 'Create invite',
    signupLink: 'Signup link',
    ledger: '/admin/invites',
  },
  signup: {
    path: '/signup',
    email: TESTIDS.signupEmail,
    submit: TESTIDS.signupSubmit,
    error: TESTIDS.signupError,
    emailCode: 'Email code',
    finish: 'Finish creating account',
  },
  library: {
    path: '/library',
    api: API.library,
    item: TESTIDS.libraryItem,
    id: 'data-id',
    refresh: 'Refresh',
    forbidden: 'You do not have access to this library.',
  },
  player: {
    audio: TESTIDS.audioPlayer,
    seek: TESTIDS.playerSeek,
    next: TESTIDS.playerNext,
    queueItem: 'queue-item',
    queueId: 'data-track-id',
    currentQueueItem: '[data-testid="queue-item"][aria-current="true"]',
    plays: '/v1/plays',
    heartbeat: '/v1/plays/heartbeat',
  },
} as const;

export function assertPrivateInvite(observed: {
  privateListening: boolean;
  destination: string;
  inviteFragmentCleared: boolean;
  libraryStatus: number;
}): void {
  if (
    observed.privateListening !== true ||
    observed.destination !== PRIVATE_MUSIC.library.path ||
    observed.inviteFragmentCleared !== true ||
    observed.libraryStatus !== 200
  ) {
    throw new Error('Private invite must open Library with a working session and a cleared invite fragment');
  }
}

export function assertCuratedLibrary(
  expectedDigests: readonly string[],
  published: readonly { sha256: string; trackId: string }[],
  libraryOrder: readonly string[],
): string[] {
  if (
    expectedDigests.length < 2 ||
    new Set(expectedDigests).size !== expectedDigests.length ||
    expectedDigests.some((digest) => !/^[a-f0-9]{64}$/.test(digest))
  ) {
    throw new Error('Choose at least two distinct FLAC digests for the curated Library check');
  }
  if (published.length !== expectedDigests.length) {
    throw new Error('Expected one publication for each uploaded FLAC');
  }
  const ids = expectedDigests.map((digest) => {
    const matches = published.filter((track) => track.sha256 === digest);
    const id = matches[0]?.trackId;
    if (matches.length !== 1 || !id?.trim()) {
      throw new Error('Each uploaded FLAC digest must identify one published track');
    }
    return id;
  });
  const selectedOrder = libraryOrder.filter((id) => ids.includes(id));
  if (
    new Set(ids).size !== ids.length ||
    selectedOrder.length !== ids.length ||
    new Set(selectedOrder).size !== ids.length
  ) {
    throw new Error('The invitee Library must render every published track exactly once');
  }
  return selectedOrder;
}

export function assertPrivateTransport(selectionOrder: string[], observed: {
  firstTrackId: string | null;
  baselineTime: number;
  playback: PlaybackEvidence;
  seek: TimedPlayer & { fraction: number; sawSeekedEvent: boolean };
  resume: { minimumTime: number; playback: PlaybackEvidence };
  queueOrder: string[] | null;
  nextTrackId: string | null;
}): void {
  // The shared queue checker includes IDs in its diagnostics. Keep the
  // private journey's diagnostics independent of stage-specific IDs.
  if (queueOrderFailure(selectionOrder, observed.queueOrder)) {
    throw new Error('Queue must match the curated Library selection in order');
  }
  if (observed.firstTrackId !== selectionOrder[0] || observed.nextTrackId !== selectionOrder[1]) {
    throw new Error('Play and Next must select the first and second curated tracks');
  }
  if (
    !Number.isFinite(observed.baselineTime) ||
    observed.baselineTime < 0 ||
    playbackFailure(observed.playback, observed.baselineTime)
  ) {
    throw new Error('Curated playback must decode and advance before seeking');
  }
  const { seek } = observed;
  if (
    !Number.isFinite(seek.currentTime) ||
    !Number.isFinite(seek.duration) ||
    seek.duration <= 0 ||
    !(seek.fraction > 0.25 && seek.fraction < 0.75) ||
    seekProofFailure(seek, seek.fraction, seek.sawSeekedEvent)
  ) {
    throw new Error('The player seek control must produce a completed mid-track seek while paused');
  }
  const { resume } = observed;
  if (
    !Number.isFinite(resume.minimumTime) ||
    resume.minimumTime < resumeFloorAfterSeek(seek.currentTime, seek.duration) ||
    resume.minimumTime > resume.playback.maxCurrentTime ||
    playbackFailure(resume.playback, seek.currentTime)
  ) {
    throw new Error('Playback must resume and advance from the sought position');
  }
}

const REFUSALS = {
  missingInviteSignup: 401,
  anonymousLibrary: 401,
  uninvitedLibrary: 403,
  uninvitedPlay: 403,
  revokedLibrary: 403,
  revokedPlay: 403,
  revokedHeartbeat: 403,
} as const;

export function assertPrivateRefusals(
  statuses: Record<keyof typeof REFUSALS, number>,
  remainingCuratedRows: number,
): void {
  for (const outcome of Object.keys(REFUSALS) as Array<keyof typeof REFUSALS>) {
    if (statuses[outcome] !== REFUSALS[outcome]) {
      throw new Error(`Private listening refusal ${outcome} must return HTTP ${REFUSALS[outcome]}`);
    }
  }
  if (remainingCuratedRows !== 0) {
    throw new Error('Refreshing Library after revocation must clear the curated rows');
  }
}
