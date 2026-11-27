import type { Locator, Page } from '@playwright/test';
import { PENDING_TESTIDS, TESTIDS } from './contract.js';

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

function validFraction(fraction: number): boolean {
  return Number.isFinite(fraction) && fraction >= 0 && fraction <= 1;
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
  if (sawSeekedEvent !== true) {
    return (
      'No "seeked" event fired after acting on the player UI seek control ' +
      '(the seek did not happen, or playback did not reach a seekable position)'
    );
  }
  if (state?.paused !== true) {
    return 'Playback did not stay paused after the seek — the state does not prove the sought position';
  }
  if (!validFraction(fraction) || !Number.isFinite(state.duration) || state.duration <= 0 ||
      !Number.isFinite(state.currentTime) || state.currentTime < 0 || state.currentTime > state.duration) {
    return 'Invalid paused seek position, duration or fraction';
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
 * when visible and enabled; the native timeline is only usable while `controls` is
 * rendered. Returns null when neither exists — the caller must fail by
 * name (via {@link seekSurfaceFailure}) rather than fake the seek.
 */
export function chooseSeekSurface(
  seekSliderUsable: boolean,
  nativeControlsPresent: boolean,
): SeekSurface | null {
  if (seekSliderUsable === true) return 'custom';
  if (nativeControlsPresent === true) return 'native';
  return null;
}

/** Named failure for a player with no user-seekable control at all. */
export function seekSurfaceFailure(
  seekSliderVisible: boolean,
  nativeControlsPresent: boolean,
): string {
  return (
    'No seekable player UI: the custom player-seek slider is unavailable ' +
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

async function clickAt(control: Locator, locate: (box: Box) => { x: number; y: number }): Promise<void> {
  await control.scrollIntoViewIfNeeded();
  const box = await control.boundingBox();
  if (!box) throw new Error('Player control has no bounding box');
  const point = locate({ ...box, x: 0, y: 0 });
  if (!(point.x > 0 && point.x < box.width && point.y > 0 && point.y < box.height) ||
      !Number.isFinite(box.width) || !Number.isFinite(box.height)) throw new Error('Invalid player control geometry');
  await control.click({ position: point });
}

/** Wait until the player's `paused` state matches `paused`. */
export async function waitForPaused(
  player: Locator,
  paused: boolean,
  timeout: number,
  message: string,
): Promise<TimedPlayer> {
  if (!Number.isSafeInteger(timeout) || timeout <= 0 || typeof paused !== 'boolean') {
    throw new Error('Pause wait requires a positive timeout and a boolean state');
  }
  const deadline = performance.now() + timeout;
  for (;;) {
    const remaining = deadline - performance.now();
    if (remaining <= 0) throw new Error(message);
    const state = await player.evaluate((el) => {
      if (!(el instanceof HTMLAudioElement)) throw new Error('Expected an audio player');
      return { currentTime: el.currentTime, duration: el.duration, paused: el.paused };
    }, undefined, { timeout: Math.ceil(remaining) });
    if (performance.now() >= deadline) throw new Error(message);
    if (state.paused === paused) return state;
    await new Promise((resolve) => setTimeout(resolve, Math.min(200, deadline - performance.now())));
  }
}

/**
 * Toggle play/pause the way a user would: the `player-play-pause` button
 * in the custom bar, or the native toggle position on the controls. Fails
 * by name when neither exists (a player with no toggle cannot pass).
 */
export async function togglePlayViaPlayerUi(page: Page, player: Locator): Promise<void> {
  const playPause = page.getByTestId(PENDING_TESTIDS.playerPlayPause);
  const count = await playPause.count();
  if (count > 1) throw new Error('Ambiguous player play/pause control');
  if (count === 1 && (await playPause.isVisible())) {
    await playPause.click();
    return;
  }
  const hasControls = await player.evaluate((el) => el instanceof HTMLAudioElement && el.controls);
  if (!hasControls) {
    throw new Error(seekSurfaceFailure(false, false));
  }
  await clickAt(player, nativeToggleClickPoint);
}

/**
 * Seek to `fraction` of the duration through the player UI and return the
 * player state after the click. The click is a real pointer press at the
 * proportional position — on the `player-seek` slider when it
 * is enabled, otherwise on the native `<audio controls>` timeline —
 * identical in kind to what a user clicking the bar does. Never sets
 * `currentTime` directly. Fails by name when no seekable control exists.
 * Seek PROOF is the caller's job (see {@link seekProofFailure}).
 */
export async function seekViaPlayerUi(
  page: Page,
  player: Locator,
  fraction: number,
): Promise<SeekTarget> {
  if (!validFraction(fraction)) throw new Error('Seek fraction must be finite and within [0, 1]');
  const slider = page.getByTestId(TESTIDS.playerSeek);
  const count = await slider.count();
  if (count > 1) throw new Error('Ambiguous player seek control');
  const sliderVisible = count === 1 && await slider.isVisible();
  const sliderEnabled = sliderVisible && (await slider.isEnabled());
  const hasControls = await player.evaluate((el) => el instanceof HTMLAudioElement && el.controls);
  const surface = chooseSeekSurface(sliderEnabled, hasControls);
  if (surface === null) {
    throw new Error(`${seekSurfaceFailure(sliderVisible, hasControls)} Custom slider enabled=${sliderEnabled}.`);
  }
  const target: SeekTarget = { fraction, surface };
  const seekControl = surface === 'custom' ? slider : player;
  await clickAt(seekControl, (box) => fractionClickPoint(box, fraction));
  return target;
}
