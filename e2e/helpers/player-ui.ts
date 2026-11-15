import type { Locator, Page } from '@playwright/test';
import { TESTIDS } from './contract.js';

/**
 * Player-UI actions for the player-contract migration.
 *
 * The custom player-bar build removes the `<audio controls>` timeline that
 * the play/seek milestone used to click. To keep the milestone honest
 * BEFORE that build lands (and green when it does), these helpers squeeze
 * the player's transport exactly the way a user would on the player UI —
 * never by setting `currentTime` programmatically — and support BOTH
 * player generations:
 *
 *   - custom player bar: `player-seek` slider (preferred when present)
 *     and `player-play-pause` button;
 *   - native controls: the timeline / toggle click positions this
 *     milestone has always used.
 *
 * Lowest-common-denominator rule (same as the rest of the journey): a
 * test that cannot prove its seek fails by name. The pure math below is
 * pinned offline by e2e/tests/player-ui.spec.ts. Failure messages carry
 * only booleans/numbers — never page URLs, titles or other detail that
 * must stay out of public CI artifacts.
 */

export interface TimedPlayer {
  currentTime: number;
  duration: number;
  paused: boolean;
}

export type SeekSurface = 'custom' | 'native';

export interface SeekTarget {
  /** Fraction of the duration to seek to (0..=1). */
  fraction: number;
  /** Which player UI to drive: seek slider, or native timeline. */
  surface: SeekSurface;
}

interface Box {
  x: number;
  y: number;
  width: number;
  height: number;
}

/**
 * Proven-seek region for a click at `fraction` of the seek control's
 * width. Deliberately wide (for the milestone's 0.55 click: 30%..95% of
 * the duration): the native timeline does not span the full element width
 * (play button and time labels flank it), so an exact timestamp cannot be
 * assumed from the click fraction. Non-vacuity comes from the seek being
 * performed while PAUSED plus the `seeked` event (see seekProofFailure),
 * not from a tight region.
 */
export function seekProofRegion(
  duration: number,
  fraction: number,
): { min: number; max: number } {
  return {
    min: duration * Math.max(0, fraction - 0.25),
    max: duration * Math.min(0.95, fraction + 0.4),
  };
}

/**
 * Resume floor: playback resumed after a PAUSED seek must never be
 * observed below this position — it continues from the seek, it does not
 * restart. Matches the live milestone's tolerance (5% of duration).
 */
export function resumeFloorAfterSeek(soughtPosition: number, duration: number): number {
  return Math.max(0, soughtPosition - duration * 0.05);
}

/**
 * Return null when the player state proves the seek to `fraction` happened
 * across the player UI — a `seeked` event fired and the still-paused
 * position landed in the proof region — otherwise a named failure.
 * Still-paused is part of the proof: while paused, only the seek itself
 * can have moved the position, and the `seeked` event proves a seek
 * completed rather than the position having started there (a distance
 * predicate could never become true when selection-autoplay pauses near
 * the click point).
 */
export function seekProofFailure(
  state: TimedPlayer,
  fraction: number,
  sawSeekedEvent: boolean,
): string | null {
  if (!sawSeekedEvent) {
    return (
      'No "seeked" event fired after acting on the player UI seek control ' +
      '(the seek did not happen, or playback did not reach a seekable position)'
    );
  }
  if (!state.paused) {
    return 'Playback did not stay paused after the seek — the state does not prove the sought position';
  }
  const region = seekProofRegion(state.duration, fraction);
  if (state.currentTime < region.min || state.currentTime > region.max) {
    return (
      `Post-seek position ${state.currentTime.toFixed(1)}s is outside the expected region ` +
      `${region.min.toFixed(1)}s..${region.max.toFixed(1)}s of a ${state.duration.toFixed(1)}s ` +
      `track for a ${fraction} seek`
    );
  }
  return null;
}

/**
 * Choose the seek surface the user would find: the custom seek slider wins
 * when present; the native timeline is only usable while `controls` is
 * rendered. Returns null when neither exists — the caller must fail by
 * name (via {@link seekSurfaceFailure}) rather than fake the seek.
 */
export function chooseSeekSurface(
  seekSliderVisible: boolean,
  nativeControlsPresent: boolean,
): SeekSurface | null {
  if (seekSliderVisible) return 'custom';
  if (nativeControlsPresent) return 'native';
  return null;
}

/** Named failure for a player with no user-seekable control at all. */
export function seekSurfaceFailure(
  seekSliderVisible: boolean,
  nativeControlsPresent: boolean,
): string {
  return (
    'No seekable player UI: the custom player-seek slider is absent ' +
    `(visible=${seekSliderVisible}) and the audio element renders no native ` +
    `controls (controls=${nativeControlsPresent}) — a non-seekable player ` +
    'cannot pass the play/seek milestone'
  );
}

/**
 * Point inside `box` at `fraction` of its width (clamped with a 1px edge
 * inset), vertically centred. Shared by both seek surfaces so the custom
 * slider and the native timeline land on the same proportional position.
 */
export function fractionClickPoint(
  box: Box,
  fraction: number,
): { x: number; y: number } {
  const clamped = Math.min(0.98, Math.max(0.02, fraction));
  const maxInset = Math.max(0, box.width - 2);
  return {
    x: box.x + 1 + Math.min(maxInset, Math.round(box.width * clamped)),
    y: box.y + box.height / 2,
  };
}

/**
 * Native-controls toggle point: 16px in from the left edge (the
 * play/pause button in Chromium's audio controls), vertically centred.
 * Clicking a control element directly (`player.click()` at the centre)
 * lands on the CONTROL at that point — for the native player Playwright's
 * default centre point is on the timeline, NOT the play button, and only
 * seeks.
 */
export function nativeToggleClickPoint(box: Box): { x: number; y: number } {
  return { x: box.x + 16, y: box.y + box.height / 2 };
}

async function playerState(player: Locator): Promise<TimedPlayer> {
  return player.evaluate((el) => {
    const a = el as HTMLAudioElement;
    return { currentTime: a.currentTime, duration: a.duration, paused: a.paused };
  });
}

/** Wait until the player's `paused` state matches `paused`. */
export async function waitForPaused(
  player: Locator,
  paused: boolean,
  timeout: number,
  message: string,
): Promise<TimedPlayer> {
  const start = Date.now();
  for (;;) {
    const state = await playerState(player);
    if (state.paused === paused) return state;
    if (Date.now() - start > timeout) {
      throw new Error(
        `${message} (paused=${state.paused}, position=${state.currentTime.toFixed(1)}s ` +
          `of ${state.duration.toFixed(1)}s)`,
      );
    }
    await new Promise((resolve) => setTimeout(resolve, 200));
  }
}

/**
 * Toggle play/pause the way a user would: the `player-play-pause` button
 * in the custom bar, or the native toggle position on the controls. Fails
 * by name when neither exists (a player with no toggle cannot pass).
 */
export async function togglePlayViaPlayerUi(page: Page, player: Locator): Promise<void> {
  const playPause = page.getByTestId(TESTIDS.playerPlayPause);
  if ((await playPause.count()) > 0 && (await playPause.isVisible())) {
    await playPause.click();
    return;
  }
  const hasControls = await player.evaluate((el) => (el as HTMLAudioElement).controls);
  if (!hasControls) {
    throw new Error(seekSurfaceFailure(false, false));
  }
  // page.mouse.click never scrolls (locator.click did): the player renders
  // below the library list, off-viewport, so bring it into view before
  // taking viewport-relative coordinates from boundingBox().
  await player.scrollIntoViewIfNeeded();
  const box = await player.boundingBox();
  if (!box) {
    throw new Error('The audio element has no bounding box — native transport controls are not rendered');
  }
  const point = nativeToggleClickPoint(box);
  await page.mouse.click(point.x, point.y);
}

/**
 * Seek to `fraction` of the duration through the player UI and return the
 * player state after the click. The click is a real pointer press at the
 * proportional position — on the `player-seek` slider when the custom bar
 * is present, otherwise on the native `<audio controls>` timeline —
 * identical in kind to what a user clicking the bar does. Never sets
 * `currentTime` directly. Fails by name when no seekable control exists.
 * Seek PROOF is the caller's job (see {@link seekProofFailure}).
 */
export async function seekViaPlayerUi(
  page: Page,
  player: Locator,
  fraction: number,
): Promise<SeekTarget> {
  const slider = page.getByTestId(TESTIDS.playerSeek);
  const sliderVisible =
    (await slider.count()) > 0 ? await slider.isVisible() : false;
  const hasControls = await player.evaluate((el) => (el as HTMLAudioElement).controls);
  const surface = chooseSeekSurface(sliderVisible, hasControls);
  if (surface === null) {
    throw new Error(seekSurfaceFailure(sliderVisible, hasControls));
  }
  const target: SeekTarget = { fraction, surface };
  const seekControl = surface === 'custom' ? slider : player;
  // Same off-viewport rule as the toggle: scroll before boundingBox().
  await seekControl.scrollIntoViewIfNeeded();
  const box = await seekControl.boundingBox();
  if (!box) {
    throw new Error(`The ${surface} seek control has no bounding box — it is not rendered`);
  }
  const point = fractionClickPoint(box, fraction);
  await page.mouse.click(point.x, point.y);
  return target;
}
