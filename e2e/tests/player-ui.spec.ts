import { test, expect } from '@playwright/test';
import {
  chooseSeekSurface,
  fractionClickPoint,
  nativeToggleClickPoint,
  resumeFloorAfterSeek,
  seekProofFailure,
  seekProofRegion,
  seekSurfaceFailure,
  seekViaPlayerUi,
  togglePlayViaPlayerUi,
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

/** A few seconds of silent PCM WAV as a data URI (no fixture file needed). */
function silentWavDataUri(seconds = 30): string {
  const sampleRate = 8000;
  const samples = sampleRate * seconds;
  const buffer = Buffer.alloc(44 + samples * 2);
  buffer.write('RIFF', 0);
  buffer.writeUInt32LE(36 + samples * 2, 4);
  buffer.write('WAVEfmt ', 8);
  buffer.writeUInt32LE(16, 16);
  buffer.writeUInt16LE(1, 20); // PCM
  buffer.writeUInt16LE(1, 22); // mono
  buffer.writeUInt32LE(sampleRate, 24);
  buffer.writeUInt32LE(sampleRate * 2, 28);
  buffer.writeUInt16LE(2, 32);
  buffer.writeUInt16LE(16, 34);
  buffer.write('data', 36);
  buffer.writeUInt32LE(samples * 2, 40);
  return `data:audio/wav;base64,${buffer.toString('base64')}`;
}

test.describe('DOM driver (off-viewport player still receives the clicks)', () => {
  // The library page renders the audio element below the track list, so on
  // a 720px-tall viewport it starts off-screen. `page.mouse.click` on raw
  // boundingBox() coordinates never scrolls (the old locator.click did),
  // so the driver must scroll the control into view first — otherwise the
  // live play/seek legs click empty space and time out. These cases pin
  // the driver itself, not just the pure verdict above. Clicks on native
  // media controls are not observable as DOM click events (internal
  // shadow tree), so the proof is the media state itself: the toggle must
  // start/stop playback and the seek must fire `seeked` and move the
  // position — none of which happens when the click misses.
  test('toggle and seek clicks land on a player rendered below the viewport', async ({
    page,
  }) => {
    await page.setContent(
      `<div style="height:3000px"></div>` +
        `<audio controls data-testid="audio-player" src="${silentWavDataUri()}"></audio>`,
    );
    const player = page.getByTestId('audio-player');
    const state = () =>
      player.evaluate((el) => {
        const a = el as HTMLAudioElement & { __sawSeeked?: boolean };
        return {
          currentTime: a.currentTime,
          duration: a.duration,
          paused: a.paused,
          readyState: a.readyState,
          sawSeeked: a.__sawSeeked === true,
        };
      });
    await expect
      .poll(async () => (await state()).readyState, {
        message: 'expected the fixture audio metadata to load',
      })
      .toBeGreaterThanOrEqual(1);

    // Non-vacuity: the player really does start outside the viewport.
    const box = await player.boundingBox();
    expect(box).not.toBeNull();
    expect(box!.y).toBeGreaterThan(page.viewportSize()!.height);
    expect((await state()).paused).toBe(true);

    await togglePlayViaPlayerUi(page, player);
    await expect
      .poll(async () => (await state()).paused, {
        message: 'expected the off-viewport toggle click to start playback',
      })
      .toBe(false);

    await togglePlayViaPlayerUi(page, player);
    await expect
      .poll(async () => (await state()).paused, {
        message: 'expected the second toggle click to pause playback again',
      })
      .toBe(true);

    await player.evaluate((el) => {
      const a = el as HTMLAudioElement & { __sawSeeked?: boolean };
      a.__sawSeeked = false;
      a.addEventListener('seeked', () => {
        a.__sawSeeked = true;
      });
    });
    const target = await seekViaPlayerUi(page, player, 0.55);
    expect(target).toEqual({ fraction: 0.55, surface: 'native' });
    await expect
      .poll(async () => (await state()).sawSeeked, {
        message: 'expected the off-viewport seek click to fire "seeked"',
      })
      .toBe(true);
    const afterSeek = await state();
    expect(afterSeek.paused).toBe(true);
    expect(
      seekProofFailure(afterSeek, 0.55, afterSeek.sawSeeked),
      'expected the DOM-driven seek to satisfy the same proof as the live milestone',
    ).toBeNull();
  });
});
