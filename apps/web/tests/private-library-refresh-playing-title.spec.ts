import { expect, type Request } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { privateStagingAudio } from '../../../e2e/helpers/private-staging-fixture.js';
import { mockInvitedLibraryPlayback } from './fixtures/invited-library-playback.js';
import { test } from './helpers/private-music-fixture.js';

test('the invited Library refreshes the selected row title during active playback', async ({ page, origin }) => {
	const { library, player } = PRIVATE_MUSIC;
	const neighbor = { id: '00000000-0000-4000-8000-000000000031', title: 'Amber Tone', duration_ms: 14_000 };
	const selected = { id: '00000000-0000-4000-8000-000000000032', title: 'Dusk Tone', duration_ms: 14_000 };
	const updated = { ...selected, title: 'Evening Tone' };
	const tone = privateStagingAudio().find(({ file }) => file === 'tone-a.flac')!;
	let listingReads = 0;
	const fixture = await mockInvitedLibraryPlayback(page, origin, {
		libraryItems: () => [neighbor, ++listingReads === 1 ? selected : updated],
		playableTrackId: (id) => id === selected.id ? id : undefined,
		audio: tone.body
	});
	const grants: Request[] = [];
	const listingRequests: string[] = [];
	const mediaRequests = new Set<string>();
	page.on('request', (request) => {
		const url = new URL(request.url());
		if (url.origin !== origin) return;
		if (url.pathname === library.api) listingRequests.push(`${request.method()} ${url.pathname}${url.search}`);
		if (url.pathname === player.plays) grants.push(request);
		if (url.pathname.startsWith(`${player.plays}/media/`)) mediaRequests.add(url.href);
	});

	const rows = page.getByTestId(library.item);
	const row = (id: string) => rows.and(page.locator(`[${library.id}="${id}"]`));
	const checkbox = (track: typeof selected) => row(track.id)
		.getByRole('checkbox', { name: `Select ${track.title}`, exact: true });
	const neighborTitle = row(neighbor.id).getByRole('button', { name: neighbor.title, exact: true });
	const selectionCount = page.getByTestId('selection-count');
	const queue = page.getByTestId(player.queueItem);
	const current = page.locator(player.currentQueueItem);
	const transport = page.getByRole('region', { name: 'Player', exact: true });
	const audio = transport.getByTestId(player.audio);

	await fixture.signIn();
	await page.goto(origin + library.path);
	await expect(rows).toHaveCount(2);
	await expect(neighborTitle).toBeVisible();
	await expect(checkbox(neighbor)).not.toBeChecked();
	await checkbox(selected).check();
	await expect(selectionCount).toHaveText('1 selected');
	await row(selected.id).getByRole('button', { name: selected.title, exact: true }).click();
	await expect(current).toHaveAttribute(player.queueId, selected.id);
	const [grant] = await Promise.all([
		page.waitForResponse(origin + player.plays),
		transport.getByRole('button', { name: 'Play', exact: true }).click()
	]);
	expect(grant.status()).toBe(200);
	const source = origin + fixture.mediaPath(selected.id);
	await expect(audio).toHaveJSProperty('currentSrc', source);
	// Keep the synthetic tone running throughout the test even on a slow runner.
	await audio.evaluate((node: HTMLAudioElement) => { node.playbackRate = 0.25; });
	await expect.poll(() => audio.evaluate((node: HTMLAudioElement) =>
		!node.paused && node.readyState >= node.HAVE_FUTURE_DATA && node.currentTime > 0.2
	)).toBe(true);
	const playback = await audio.evaluateHandle((node: HTMLAudioElement) => {
		const interruptions: string[] = [];
		for (const event of ['pause', 'waiting', 'seeking', 'emptied', 'loadstart', 'ended', 'error']) {
			node.addEventListener(event, () => interruptions.push(event));
		}
		return { node, interruptions };
	});
	await expect(row(selected.id).getByRole('button', { name: `${selected.title} — Queued`, exact: true }))
		.toHaveText(`${selected.title} Queued`, { useInnerText: true });
	expect(listingReads).toBe(1);

	const [listing] = await Promise.all([
		page.waitForResponse(origin + library.api),
		page.getByRole('button', { name: library.refresh, exact: true }).click()
	]);
	expect(listing.status()).toBe(200);
	const refreshedTitle = row(updated.id).getByRole('button', { name: `${updated.title} — Queued`, exact: true });
	await expect(refreshedTitle).toBeVisible();
	await expect(refreshedTitle).toHaveText(`${updated.title} Queued`, { useInnerText: true });
	await expect(row(selected.id)).not.toContainText(selected.title, { useInnerText: true });
	await expect(rows).toHaveCount(2);
	await expect(checkbox(updated)).toBeVisible();
	await expect(checkbox(updated)).toBeChecked();
	await expect(neighborTitle).toBeVisible();
	await expect(checkbox(neighbor)).toBeVisible();
	await expect(checkbox(neighbor)).not.toBeChecked();
	await expect(selectionCount).toHaveText('1 selected');
	await expect(queue).toHaveCount(1);
	await expect(current).toHaveAttribute(player.queueId, selected.id);
	await expect(transport.getByRole('button', { name: 'Pause', exact: true })).toBeVisible();

	const position = await playback.evaluate(({ node }) => node.currentTime);
	await expect.poll(() => audio.evaluate((node: HTMLAudioElement, { original, position }) => ({
		sameElement: node === original.node,
		source: node.currentSrc,
		advancing: !node.paused && !node.ended && !node.error && node.currentTime > position + 0.1,
		interruptions: original.interruptions
	}), { original: playback, position }), 'the original audio keeps advancing after Refresh').toEqual({
		sameElement: true, source, advancing: true, interruptions: []
	});
	await playback.dispose();
	expect(grants.map((request) => ({ method: request.method(), trackId: request.postDataJSON()?.track_id })))
		.toEqual([{ method: 'POST', trackId: selected.id }]);
	expect([...mediaRequests]).toEqual([source]);
	expect(listingReads).toBe(2);
	expect(listingRequests).toEqual([`GET ${library.api}`, `GET ${library.api}`]);
});
