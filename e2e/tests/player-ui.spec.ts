import { test, expect } from '@playwright/test';
import {
  chooseSeekSurface,
  fractionClickPoint,
  nativeToggleClickPoint,
  resumeFloorAfterSeek,
  seekProofFailure,
  seekProofRegion,
  seekSurfaceFailure,
  type TimedPlayer,
} from '../helpers/player-ui.js';

// Offline acceptance for the player-UI seek verdict used by the live
// "player UI seek moves the active audio position" spec in
// ingest-play-meter.spec.ts. No staging, no credentials, no skip guard:
// these cases pin that the migration from the native <audio controls>
// timeline to the custom player bar can never turn a failed, absent or
// restarted seek into a passing proof, whichever surface drove it.

/** 60s track, paused at 32s after a 55%-of-width seek — a good seek. */
function goodState(overrides: Partial<TimedPlayer> = {}): TimedPlayer {
  return { currentTime: 32, duration: 60, paused: true, ...overrides };
}

test.describe('seekProofFailure (player-UI seek verdict, offline fixtures)', () => {
  test('a seek landing in the proof region passes', () => {
    expect(seekProofFailure(goodState(), 0.55, true)).toBeNull();
  });

  test('a missing seeked event fails by name', () => {
    const failure = seekProofFailure(goodState(), 0.55, false);
    expect(failure).not.toBeNull();
    expect(failure).toMatch(/seeked/);
  });

  test('playback resuming on its own after the seek fails', () => {
    expect(seekProofFailure(goodState({ paused: false }), 0.55, true)).toMatch(/paused/);
  });

  test('a position below the proof region fails', () => {
    expect(seekProofFailure(goodState({ currentTime: 10 }), 0.55, true)).toMatch(/outside/);
  });

  test('a position above the proof region fails', () => {
    expect(seekProofFailure(goodState({ currentTime: 58 }), 0.55, true)).toMatch(/outside/);
  });

  test('the region reproduces the milestone 0.55-click window (30%..95%)', () => {
    const region = seekProofRegion(60, 0.55);
    expect(region.min).toBeCloseTo(18);
    expect(region.max).toBeCloseTo(57);
  });

  test('the region tracks the fraction for other click points', () => {
    const region = seekProofRegion(120, 0.25);
    expect(region.min).toBe(0);
    expect(region.max).toBeCloseTo(78);
    expect(
      seekProofFailure({ currentTime: 30, duration: 120, paused: true }, 0.25, true),
    ).toBeNull();
  });
});

test.describe('resumeFloorAfterSeek (no-restart floor)', () => {
  test('the floor is 5% of duration under the sought position', () => {
    expect(resumeFloorAfterSeek(32, 60)).toBe(29);
  });

  test('the floor never goes negative for very early seeks', () => {
    expect(resumeFloorAfterSeek(0.4, 60)).toBe(0);
  });
});

test.describe('chooseSeekSurface (which player UI drives the seek)', () => {
  test('the custom seek slider wins when visible', () => {
    expect(chooseSeekSurface(true, true)).toBe('custom');
    expect(chooseSeekSurface(true, false)).toBe('custom');
  });

  test('the native timeline is used while only controls exist', () => {
    expect(chooseSeekSurface(false, true)).toBe('native');
  });

  test('no surface at all must fail by name, never fake a seek', () => {
    expect(chooseSeekSurface(false, false)).toBeNull();
    const failure = seekSurfaceFailure(false, false);
    expect(failure).toMatch(/No seekable player UI/);
    expect(failure).toContain('visible=false');
    expect(failure).toContain('controls=false');
  });
});

test.describe('click points (pointer geometry for both surfaces)', () => {
  const box = { x: 100, y: 200, width: 400, height: 20 };

  test('the fraction point is proportional and vertically centred', () => {
    const point = fractionClickPoint(box, 0.55);
    expect(point.x).toBeGreaterThan(box.x + 200);
    expect(point.x).toBeLessThan(box.x + 240);
    expect(point.y).toBe(box.y + 10);
  });

  test('the fraction point stays inside the box at the extremes', () => {
    for (const fraction of [0, 1, 2]) {
      const point = fractionClickPoint(box, fraction);
      expect(point.x).toBeGreaterThan(box.x);
      expect(point.x).toBeLessThan(box.x + box.width);
    }
  });

  test('the native toggle point is 16px in, vertically centred', () => {
    expect(nativeToggleClickPoint(box)).toEqual({ x: 116, y: 210 });
  });
});
