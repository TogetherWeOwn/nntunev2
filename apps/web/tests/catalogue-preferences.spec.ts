import { expect, test, type Page, type Route } from '@playwright/test';

// Mocked contract coverage for album save and artist follow toggles. The
// backend is not delivered when this is written, so these mocks are NOT live
// acceptance: they pin the browser behaviour against the agreed contract only.
//   GET|PUT|DELETE /v1/me/library/albums/{mbid}  -> { saved, saved_at }
//   GET|PUT|DELETE /v1/me/library/artists/{mbid} -> { followed, followed_at }
// PUT/DELETE are bodiless and credentialed.

const albumId = 'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa';
const artistId = 'bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb';
const trackId = 'cccccccc-cccc-cccc-cccc-cccccccccccc';
const groupId = 'dddddddd-dddd-dddd-dddd-dddddddddddd';
const release = { mbid: albumId, title: 'Quiet Rooms', release_type: 'Album', year: 2024,
	artist_mbid: artistId, artist: 'North Window', art_url: `/v1/releases/${albumId}/art`, release_group_mbid: groupId };
const artist = { mbid: artistId, name: 'North Window', sort_name: 'North Window', type: 'Group', country: null };
const track = { mbid: trackId, title: 'First Light', release_mbid: albumId, release: release.title,
	artist_mbid: artistId, artist: release.artist, duration_ms: 90_000, track_no: 1, medium_no: 1, art_url: release.art_url };
const transitionKey = 'nntune.library.auth-transition.v1';

async function catalogue(page: Page) {
	await page.route('**/v1/releases/*/art', (route) => route.fulfill({ status: 404, body: '' }));
	await page.route('**/v1/releases/*', (route) => route.fulfill({ contentType: 'application/json', body: JSON.stringify({ release, tracks: [track] }) }));
	await page.route('**/v1/artists/*', (route) => route.fulfill({ contentType: 'application/json', body: JSON.stringify({ artist, releases: [release], truncated: false }) }));
}

type Entity = {
	name: 'album' | 'artist';
	id: string;
	url: string;
	heading: string;
	segment: 'albums' | 'artists';
	flag: 'saved' | 'followed';
	stamp: 'saved_at' | 'followed_at';
	off: string;
	on: string;
	retry: string;
};
const entities: Entity[] = [
	{ name: 'album', id: albumId, url: `/album/${albumId}`, heading: 'Quiet Rooms', segment: 'albums', flag: 'saved', stamp: 'saved_at', off: 'Save album', on: 'Saved', retry: 'Retry saved status' },
	{ name: 'artist', id: artistId, url: `/artist/${artistId}`, heading: 'North Window', segment: 'artists', flag: 'followed', stamp: 'followed_at', off: 'Follow', on: 'Following', retry: 'Retry follow status' }
];

type Method = 'GET' | 'PUT' | 'DELETE';
type Reply = { status?: number; body?: unknown; raw?: string };

// Stateful stand-in for the preference endpoint. `intercept` lets a test fail
// or hold a given request; returning undefined falls through to server state.
class Prefs {
	on = false;
	log: Method[] = [];
	bodies: Array<string | null> = [];
	intercept?: (method: Method, index: number) => Promise<Reply | undefined> | Reply | undefined;
	constructor(private readonly e: Entity, private readonly onRequest?: () => void) {}
	payload(on = this.on) {
		return { [this.e.flag]: on, [this.e.stamp]: on ? '2026-01-02T03:04:05Z' : null };
	}
	count(method: Method) { return this.log.filter((m) => m === method).length; }
	async handle(route: Route) {
		const method = route.request().method() as Method;
		this.log.push(method);
		this.bodies.push(route.request().postData());
		this.onRequest?.();
		const reply = await this.intercept?.(method, this.log.length - 1);
		if (reply?.raw !== undefined) return route.fulfill({ status: reply.status ?? 200, contentType: 'application/json', body: reply.raw });
		if (reply) return route.fulfill({ status: reply.status ?? 200, contentType: 'application/json', body: JSON.stringify(reply.body ?? {}) });
		if (method === 'PUT') this.on = true;
		if (method === 'DELETE') this.on = false;
		return route.fulfill({ contentType: 'application/json', body: JSON.stringify(this.payload()) });
	}
}

async function mount(page: Page, e: Entity) {
	await catalogue(page);
	const prefs = new Prefs(e);
	await page.route(`**/v1/me/library/${e.segment}/*`, (route) => prefs.handle(route));
	return prefs;
}

function gate() {
	let open!: () => void;
	const promise = new Promise<void>((resolve) => { open = resolve; });
	return { promise, open };
}

const toggle = (page: Page, e: Entity) => page.getByRole('button', { name: new RegExp(`^(${e.off}|${e.on})$`) });
const heading = (page: Page, e: Entity) => page.getByRole('heading', { level: 1, name: e.heading });
const signIn = (page: Page) => page.getByRole('article').getByRole('link', { name: 'Sign in', exact: true });
const errorBody = (code: string) => ({ code, message: 'diagnostic', retryable: false });

async function expectOff(page: Page, e: Entity) {
	const button = page.getByRole('button', { name: e.off, exact: true });
	await expect(button).toBeEnabled();
	await expect(button).toHaveAttribute('aria-pressed', 'false');
}
async function expectOn(page: Page, e: Entity) {
	const button = page.getByRole('button', { name: e.on, exact: true });
	await expect(button).toBeEnabled();
	await expect(button).toHaveAttribute('aria-pressed', 'true');
}
// No enabled toggle may assert a confirmed value when the status is unknown.
async function expectNoEnabledToggle(page: Page, e: Entity) {
	await expect(toggle(page, e).and(page.locator(':enabled'))).toHaveCount(0);
}

for (const e of entities) {
	test.describe(`${e.name} preference toggle`, () => {
		test('initial GET disables the toggle, then PUT and DELETE confirm without optimism', async ({ page }) => {
			const prefs = await mount(page, e);
			const initial = gate();
			const write = gate();
			prefs.intercept = async (method) => {
				if (method === 'GET') await initial.promise;
				else await write.promise;
				return undefined;
			};
			await page.goto(e.url);
			await expect(heading(page, e)).toBeVisible();
			await expect.poll(() => prefs.count('GET')).toBe(1);
			await expectNoEnabledToggle(page, e);
			initial.open();
			await expectOff(page, e);

			await page.getByRole('button', { name: e.off, exact: true }).click();
			// Pending: disabled and unchanged until the server confirms.
			const pending = page.getByRole('button', { name: e.off, exact: true });
			await expect(pending).toBeDisabled();
			await expect(pending).toHaveAttribute('aria-pressed', 'false');
			await expect(page.getByRole('button', { name: e.on, exact: true })).toHaveCount(0);
			await expect.poll(() => prefs.count('PUT')).toBe(1);
			write.open();
			await expectOn(page, e);
			expect(prefs.bodies[prefs.log.indexOf('PUT')]).toBeNull();

			const removal = gate();
			prefs.intercept = async () => { await removal.promise; return undefined; };
			await page.getByRole('button', { name: e.on, exact: true }).click();
			const removing = page.getByRole('button', { name: e.on, exact: true });
			await expect(removing).toBeDisabled();
			await expect(removing).toHaveAttribute('aria-pressed', 'true');
			await expect.poll(() => prefs.count('DELETE')).toBe(1);
			removal.open();
			await expectOff(page, e);
			expect(prefs.bodies[prefs.log.indexOf('DELETE')]).toBeNull();
		});

		test('confirmed value persists across reload', async ({ page }) => {
			const prefs = await mount(page, e);
			await page.goto(e.url);
			await page.getByRole('button', { name: e.off, exact: true }).click();
			await expectOn(page, e);
			await page.reload();
			await expectOn(page, e);
			expect(prefs.on).toBe(true);
			await page.getByRole('button', { name: e.on, exact: true }).click();
			await expectOff(page, e);
			await page.reload();
			await expectOff(page, e);
		});

		test('toggle is operable by keyboard', async ({ page }) => {
			await mount(page, e);
			await page.goto(e.url);
			await expectOff(page, e);
			await page.getByRole('button', { name: e.off, exact: true }).focus();
			await page.keyboard.press('Enter');
			await expectOn(page, e);
			await page.getByRole('button', { name: e.on, exact: true }).focus();
			await page.keyboard.press('Space');
			await expectOff(page, e);
		});

		test('rapid activation sends one request', async ({ page }) => {
			const prefs = await mount(page, e);
			const write = gate();
			await page.goto(e.url);
			await expectOff(page, e);
			prefs.intercept = async (method) => { if (method !== 'GET') await write.promise; return undefined; };
			// Dispatch several activations in one task, before any re-render can disable the control.
			await page.getByRole('button', { name: e.off, exact: true }).evaluate((el) => {
				for (let i = 0; i < 4; i += 1) (el as HTMLButtonElement).click();
			});
			await expect.poll(() => prefs.count('PUT')).toBe(1);
			await page.getByRole('button', { name: e.off, exact: true }).evaluate((el) => (el as HTMLButtonElement).click());
			write.open();
			await expectOn(page, e);
			expect(prefs.count('PUT')).toBe(1);
			expect(prefs.count('DELETE')).toBe(0);
		});

		test('failed write never shows success, reconciles with a GET and retry re-reads first', async ({ page }) => {
			const prefs = await mount(page, e);
			await page.goto(e.url);
			await expectOff(page, e);
			prefs.intercept = (method) => method === 'PUT' ? { status: 503, body: errorBody('unavailable') } : undefined;
			await page.getByRole('button', { name: e.off, exact: true }).click();
			const retry = page.getByRole('button', { name: e.retry, exact: true });
			await expect(retry).toBeVisible();
			await expect(page.getByRole('button', { name: e.on, exact: true })).toHaveCount(0);
			// Whatever the UI shows must be the last confirmed value.
			await expect(page.getByRole('button', { name: e.off, exact: true })).toHaveAttribute('aria-pressed', 'false');
			// No blind mutation replay: reconciliation starts only on explicit retry.
			expect(prefs.log).toEqual(['GET', 'PUT']);

			// Another device changes the server value meanwhile; retry must re-GET before any mutation.
			prefs.on = true;
			prefs.intercept = undefined;
			const before = prefs.log.length;
			await retry.click();
			await expect.poll(() => prefs.log.length).toBeGreaterThan(before);
			expect(prefs.log[before]).toBe('GET');
			await expect(page.getByRole('button', { name: e.retry, exact: true })).toHaveCount(0);
			await expect(page.getByRole('button', { name: e.on, exact: true })).toHaveAttribute('aria-pressed', 'true');
		});

		test('failed delete preserves the confirmed value', async ({ page }) => {
			const prefs = await mount(page, e);
			prefs.on = true;
			await page.goto(e.url);
			await expectOn(page, e);
			prefs.intercept = (method) => method === 'DELETE' ? { status: 500, body: errorBody('internal') } : undefined;
			await page.getByRole('button', { name: e.on, exact: true }).click();
			await expect(page.getByRole('button', { name: e.retry, exact: true })).toBeVisible();
			await expect(page.getByRole('button', { name: e.off, exact: true })).toHaveCount(0);
			await expect(page.getByRole('button', { name: e.on, exact: true })).toHaveAttribute('aria-pressed', 'true');
		});

		test('401 shows sign-in and offers no enabled mutation', async ({ page }) => {
			const prefs = await mount(page, e);
			prefs.intercept = () => ({ status: 401, body: errorBody('unauthorized') });
			await page.goto(e.url);
			await expect(page.getByRole('link', { name: 'Sign in', exact: true }).first()).toHaveAttribute('href', '/login');
			await expectNoEnabledToggle(page, e);
			expect(prefs.count('PUT') + prefs.count('DELETE')).toBe(0);
		});

		test('404 leaves status unknown instead of pretending not saved', async ({ page }) => {
			const prefs = await mount(page, e);
			prefs.intercept = () => ({ status: 404, body: errorBody('not_found') });
			await page.goto(e.url);
			await expect.poll(() => prefs.count('GET')).toBeGreaterThan(0);
			await expect(heading(page, e)).toBeVisible();
			await expectNoEnabledToggle(page, e);
			await expect(page.getByRole('button', { name: e.off, exact: true }).and(page.locator('[aria-pressed="false"]'))).toHaveCount(0);
			expect(prefs.count('PUT') + prefs.count('DELETE')).toBe(0);
		});

		test('409 account_deleting blocks further mutations', async ({ page }) => {
			const prefs = await mount(page, e);
			await page.goto(e.url);
			await expectOff(page, e);
			prefs.intercept = (method) => method === 'GET' ? undefined : { status: 409, body: errorBody('account_deleting') };
			await page.getByRole('button', { name: e.off, exact: true }).click();
			await expect.poll(() => prefs.count('PUT')).toBe(1);
			await expect(page.getByRole('button', { name: e.on, exact: true })).toHaveCount(0);
			await expectNoEnabledToggle(page, e);
			// Forced activation must not start another write.
			await expect(toggle(page, e)).toBeDisabled();
			await toggle(page, e).evaluate((el) => (el as HTMLButtonElement).click());
			await page.keyboard.press('Enter');
			expect(prefs.count('PUT')).toBe(1);
			expect(prefs.count('DELETE')).toBe(0);
		});

		test('malformed 2xx read fails unknown and retry re-reads', async ({ page }) => {
			const prefs = await mount(page, e);
			const bad: Reply[] = [
				{ raw: '{"unexpected":true}' },
				{ body: { [e.flag]: 'yes', [e.stamp]: null } },
				{ body: { [e.flag]: false, [e.name === 'album' ? 'savedAt' : 'followedAt']: null } },
				{ raw: 'not json' }
			];
			for (const reply of bad) {
				prefs.log.length = 0;
				prefs.intercept = () => reply;
				await page.goto(e.url);
				const retry = page.getByRole('button', { name: e.retry, exact: true });
				await expect(retry).toBeVisible();
				await expectNoEnabledToggle(page, e);
				prefs.intercept = undefined;
				await retry.click();
				await expectOff(page, e);
				expect(prefs.log.every((m) => m === 'GET')).toBe(true);
			}
		});

		test('malformed 2xx write does not claim success', async ({ page }) => {
			const prefs = await mount(page, e);
			await page.goto(e.url);
			await expectOff(page, e);
			prefs.intercept = (method) => method === 'PUT' ? { raw: '{"nope":1}' } : undefined;
			await page.getByRole('button', { name: e.off, exact: true }).click();
			await expect(page.getByRole('button', { name: e.retry, exact: true })).toBeVisible();
			await expect(page.getByRole('button', { name: e.on, exact: true })).toHaveCount(0);
		});

		test('old-account GET completing after a delivered auth transition is discarded', async ({ page }) => {
			const prefs = await mount(page, e);
			const held = gate();
			prefs.intercept = async (method) => { if (method === 'GET') await held.promise; return { body: prefs.payload(true) }; };
			await page.goto(e.url, { waitUntil: 'domcontentloaded' });
			await expect.poll(() => prefs.count('GET')).toBe(1);
			await page.evaluate((key) => window.dispatchEvent(new StorageEvent('storage', { key, newValue: 'changed-account' })), transitionKey);
			held.open();
			await expect(signIn(page)).toBeVisible();
			await expect(page.getByRole('button', { name: e.on, exact: true })).toHaveCount(0);
			await expectNoEnabledToggle(page, e);
		});

		test('old-account PUT completing after a delivered auth transition is discarded', async ({ page }) => {
			const prefs = await mount(page, e);
			await page.goto(e.url);
			await expectOff(page, e);
			const held = gate();
			prefs.intercept = async (method) => { if (method === 'PUT') await held.promise; return undefined; };
			await page.getByRole('button', { name: e.off, exact: true }).click();
			await expect.poll(() => prefs.count('PUT')).toBe(1);
			await page.evaluate((key) => window.dispatchEvent(new StorageEvent('storage', { key, newValue: 'changed-account' })), transitionKey);
			held.open();
			await expect(signIn(page)).toBeVisible();
			await expect(page.getByRole('button', { name: e.on, exact: true })).toHaveCount(0);
			await expectNoEnabledToggle(page, e);
		});

		test('real sibling-tab auth transition clears the displayed value', async ({ page, context }) => {
			await mount(page, e);
			await page.goto(e.url);
			await expectOff(page, e);
			const sibling = await context.newPage();
			await sibling.goto('/login');
			await sibling.evaluate((key) => localStorage.setItem(key, crypto.randomUUID()), transitionKey);
			await expect(signIn(page)).toBeVisible();
			await expectNoEnabledToggle(page, e);
			await sibling.close();
		});
	});
}

test('stale album status does not leak into the artist reached by client-side navigation', async ({ page }) => {
	const [album, artistEntity] = entities;
	const albumPrefs = await mount(page, album);
	const artistPrefs = new Prefs(artistEntity);
	await page.route('**/v1/me/library/artists/*', (route) => artistPrefs.handle(route));
	const held = gate();
	albumPrefs.intercept = async () => { await held.promise; return { body: albumPrefs.payload(true) }; };
	await page.goto(album.url, { waitUntil: 'domcontentloaded' });
	await expect.poll(() => albumPrefs.count('GET')).toBe(1);
	await expect(heading(page, album)).toBeVisible();
	await page.getByRole('link', { name: 'North Window', exact: true }).click();
	await expect(page).toHaveURL(artistEntity.url);
	await expect(heading(page, artistEntity)).toBeVisible();
	await expectOff(page, artistEntity);
	held.open();
	await expect(page.getByRole('button', { name: 'Saved', exact: true })).toHaveCount(0);
	await expectOff(page, artistEntity);
	await expect(page.getByRole('button', { name: 'Following', exact: true })).toHaveCount(0);
});
