import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { fixtureCookie, hasFixtureSession, test } from './helpers/private-music-fixture.js';

test('the invited Library queues the admitted track chosen from Unknown artist', async ({
	page, context, origin
}) => {
	const { library, player } = PRIVATE_MUSIC;
	const listener = 'synthetic-listener';
	const binding = 'A'.repeat(43);
	const verifierPath = '/v1/me/likes/session';
	const target = {
		id: '00000000-0000-4000-8000-000000000001',
		occurrence_key: 'private:00000000-0000-4000-8000-000000000001',
		title: 'Synthetic Prelude',
		artist: null,
		like_target_id: null
	};
	const otherAdmission = {
		id: '00000000-0000-4000-8000-000000000002',
		occurrence_key: 'private:00000000-0000-4000-8000-000000000002',
		title: 'Synthetic Interlude',
		artist: null,
		like_target_id: null
	};
	const willowTrack = { id: '00000000-0000-4000-8000-000000000003', title: 'Synthetic Finale', artist: 'Willow' };
	const alderTrack = { id: '00000000-0000-4000-8000-000000000004', title: 'Synthetic Reprise', artist: 'Alder' };
	// The Library endpoint returns private admissions before saved catalogue rows.
	const tracks = [target, otherAdmission, willowTrack, alderTrack];
	const playbackRequests: string[] = [];

	await context.route('**/*', async (route) => {
		const request = route.request();
		const url = new URL(request.url());
		const path = url.pathname;
		if (path === player.plays || path.startsWith(`${player.plays}/`) || request.resourceType() === 'media') {
			playbackRequests.push(path);
			return route.abort();
		}
		if (url.origin !== origin || request.method() !== 'GET') return route.abort();
		if (path === library.api || path === verifierPath) {
			if (!hasFixtureSession(await request.headerValue('cookie'), listener)) return route.fulfill({ status: 401 });
			return path === library.api
				? route.fulfill({ headers: { 'x-nntune-session-binding': binding }, json: tracks })
				: route.fulfill({ json: { session_binding: binding } });
		}
		if (path.startsWith('/api/') || path.startsWith('/v1/')) return route.fulfill({ status: 404 });
		return route.continue();
	});
	await context.addCookies([{ name: fixtureCookie, value: listener, url: origin, httpOnly: true }]);

	await page.goto(origin + library.path);
	const toggle = page.getByRole('button', { name: 'Group by artist', exact: true });
	const list = page.getByTestId('library-list');
	const rows = list.getByTestId(library.item);
	const headings = list.getByRole('heading');
	const headingAndRowOrder = () => headings.or(rows).evaluateAll((nodes, idAttribute) =>
		nodes.map((node) => node.getAttribute(idAttribute) ?? node.textContent?.trim()), library.id
	);
	await expect(toggle).toHaveAttribute('aria-pressed', 'false');
	await expect.poll(headingAndRowOrder).toEqual(tracks.map((track) => track.id));

	const queue = page.getByTestId(player.queueItem);
	await expect(queue).toHaveCount(0);
	await toggle.click();
	await expect(toggle).toHaveAttribute('aria-pressed', 'true');
	await expect.poll(headingAndRowOrder).toEqual([
		alderTrack.artist, alderTrack.id, willowTrack.artist, willowTrack.id,
		'Unknown artist', target.id, otherAdmission.id
	]);
	await expect(queue).toHaveCount(0);

	await rows.getByRole('button', { name: `More actions for ${target.title}`, exact: true }).click();
	await page.getByRole('menuitem', { name: 'Add to queue', exact: true }).click();
	await expect(queue).toHaveCount(1);
	await expect(queue).toBeVisible();
	await expect(queue).toHaveAttribute(player.queueId, target.id);
	await expect(page.getByTestId('queue-announcement')).toHaveText('Added 1 track (0 already queued)');
	await expect(page.locator('audio[src]:not([src=""]), audio source[src]:not([src=""])')).toHaveCount(0);
	expect(playbackRequests).toEqual([]);
});
