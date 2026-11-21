import { expect, test, type Page, type Route } from '@playwright/test';

test.use({ trace: 'off', screenshot: 'off', video: 'off' });
const path = '/admin/invites/tree', root = 'root-user';
const branch = `branch-${'b'.repeat(121)}`;
const leaf = 'leaf-user';
const cursor = 'opaque_cursor';
const ledger = '/v1/admin/invites';
const generationKey = 'nntune.auth.generation.v1';
test.beforeEach(({ page }) => page.addInitScript((key) => localStorage.setItem(key, 'test-session'), generationKey));
const tree = (id: string) => `/v1/admin/users/${id}/invite-tree`;
const result = (id: string, inviter: string | null, children: string[], next: string | null = null) => ({
	user_id: id, inviter_id: inviter, invitee_ids: children, next_cursor: next
});
const list = (page: Page) => page.getByRole('list', { name: 'Direct invitees' });
const more = (page: Page) => page.getByRole('button', { name: 'Load more' });
const selected = (page: Page) => page.getByRole('heading', { name: 'Selected user' });
const lookup = async (page: Page, id: string) => {
	const input = page.getByLabel('Exact user ID');
	await input.fill(id);
	await input.press('Enter');
};

async function mock(page: Page, respond: (route: Route, target: string) => Promise<void>) {
	const calls: string[] = [];
	await page.route('**/v1/**', async (route) => {
		const request = route.request(), { pathname, search } = new URL(request.url());
		expect(request.method()).toBe('GET');
		expect(await request.headerValue('authorization')).toBeNull();
		const target = pathname + search;
		calls.push(target);
		if (target === ledger) return route.fulfill({ json: { invites: [] } });
		expect(pathname).toMatch(/^\/v1\/admin\/users\/[\w-]+\/invite-tree$/);
		await respond(route, target);
	});
	return calls;
}

test('walks one invite edge at a time by keyboard at 320px, from the existing ledger', async ({ page }) => {
	await page.setViewportSize({ width: 320, height: 800 });
	const calls = await mock(page, (route, target) => route.fulfill({ json: target === tree(root)
		? result(root, null, [branch]) : target === tree(branch) ? result(branch, root, [leaf]) : result(leaf, branch, []) }));
	await page.goto('/admin/invites');
	await expect(page.getByRole('link', { name: 'Invite windows' })).toBeVisible();
	await expect(page.getByRole('link', { name: 'Create an invite' })).toBeVisible();
	await page.getByRole('link', { name: 'Invite tree', exact: true }).press('Enter');
	await lookup(page, `${root}/other`);
	await expect(page.getByRole('alert')).toContainText('Enter an exact user ID');
	expect(calls).toEqual([ledger]);
	await lookup(page, root);
	await expect(selected(page)).toBeFocused();
	await expect(page.getByText('No inviter recorded.')).toBeVisible();
	await expect(list(page).getByRole('button')).toHaveText([branch]);
	await expect(page.getByText(leaf, { exact: true })).toHaveCount(0);
	expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(320);
	await page.keyboard.press('Tab');
	await expect(list(page).getByRole('button', { name: branch })).toBeFocused();
	await page.keyboard.press('Enter');
	await expect(selected(page)).toBeFocused();
	await expect(page.getByRole('button', { name: root })).toBeVisible();
	await list(page).getByRole('button', { name: leaf }).press('Enter');
	await expect(page.getByText('No direct invitees.')).toBeVisible();
	await expect(page.getByRole('button', { name: branch })).toBeVisible();
	await expect(page.getByText(root, { exact: true })).toHaveCount(0);
	await page.getByRole('button', { name: branch }).press('Enter');
	await expect(list(page).getByRole('button', { name: leaf })).toBeVisible();
	await page.getByRole('link', { name: 'Back to invite ledger' }).press('Enter');
	await expect(page.getByRole('link', { name: 'Create an invite' })).toBeVisible();
	expect(calls).toEqual([ledger, tree(root), tree(branch), tree(leaf), tree(branch), ledger]);
});

test('loads one continuation at a time, deduplicates rows and stops at a null cursor', async ({ page }) => {
	const hold = Promise.withResolvers<void>();
	const full = Array.from({ length: 50 }, (_, i) => `invitee-${i}`);
	const next = `${tree(root)}?cursor=${cursor}`;
	const calls = await mock(page, async (route, target) => {
		if (target === next) await hold.promise;
		await route.fulfill({ json: target === next ? result(root, null, [full[49], leaf]) : result(root, null, full, cursor) });
	});
	await page.goto(path);
	const storage = () => page.evaluate(() => [Object.entries(localStorage), Object.entries(sessionStorage)]);
	const before = await storage();
	await lookup(page, root);
	await expect(list(page).getByRole('listitem')).toHaveCount(50);
	await more(page).press('Enter');
	await expect(more(page)).toBeDisabled();
	await page.keyboard.press('Enter');
	hold.resolve();
	await expect(list(page).getByRole('listitem')).toHaveCount(51);
	await expect(page.getByRole('button', { name: full[49], exact: true })).toHaveCount(1);
	await expect(more(page)).toHaveCount(0);
	expect(calls).toEqual([tree(root), next]);
	expect(await storage()).toEqual(before);
});

for (const continuation of [false, true]) test(`ignores a late ${continuation ? 'continuation' : 'lookup'} after switching nodes`, async ({ page }) => {
	const delayed = continuation ? `${tree(root)}?cursor=${cursor}` : tree(root);
	await page.addInitScript((delayed) => {
		const fetchNormally = window.fetch.bind(window);
		window.fetch = async (input, init) => {
			if (!String(input).endsWith(delayed)) return fetchNormally(input, init);
			const response = await fetchNormally(input, { ...init, signal: null });
			Reflect.set(window, 'lateInviteResponse', response);
			return response;
		};
	}, delayed);
	const hold = Promise.withResolvers<void>();
	const calls = await mock(page, async (route, target) => {
		if (target === delayed) await hold.promise;
		const json = target === tree(branch) ? result(branch, null, []) : target === delayed
			? result(root, null, [leaf]) : result(root, null, [branch], cursor);
		await route.fulfill({ json });
	});
	await page.goto(path);
	if (continuation) await lookup(page, root);
	const started = page.waitForRequest((request) => request.url().endsWith(delayed));
	if (continuation) await more(page).press('Enter');
	else await lookup(page, root);
	await started;
	if (continuation) await list(page).getByRole('button', { name: branch }).press('Enter');
	else await lookup(page, branch);
	await expect(page.getByText('No direct invitees.')).toBeVisible();
	hold.resolve();
	// Wait for JS delivery, then verify the client discarded it without reading its body.
	await page.waitForFunction(() => !!Reflect.get(window, 'lateInviteResponse'));
	expect(await page.evaluate(() => Reflect.get(window, 'lateInviteResponse').bodyUsed)).toBe(false);
	await expect(page.getByText(branch, { exact: true })).toBeVisible();
	await expect(page.getByText(root, { exact: true }).or(page.getByText(leaf, { exact: true }))).toHaveCount(0);
	expect(calls).toEqual(continuation ? [tree(root), delayed, tree(branch)] : [delayed, tree(branch)]);
});

for (const status of [0, 422, 401, 403, 404, 409]) test(`clears IDs after a failed continuation (${status || 'network'})`, async ({ page }) => {
	const next = `${tree(root)}?cursor=${cursor}`;
	if ([401, 403, 404, 409].includes(status)) {
		// Route fixtures supply the HTTP status; replace denial bodies with streams that never close.
		await page.addInitScript((next) => {
			const fetchNormally = window.fetch.bind(window);
			window.fetch = async (input, init) => {
				const response = await fetchNormally(input, init);
				return String(input).endsWith(next) ? new Response(new ReadableStream(), { status: response.status }) : response;
			};
		}, next);
	}
	const calls = await mock(page, (route, target) => target === next
		? status ? route.fulfill({ status, json: { message: 'untrusted error body' } }) : route.abort('failed')
		: route.fulfill({ json: result(root, null, [leaf], cursor) }));
	await page.goto(path);
	await lookup(page, root);
	await more(page).press('Enter');
	await expect(page.getByRole('alert')).toContainText(status === 422 ? 'Restart from the first page' : status === 0 ? 'Try again' : 'unavailable');
	await expect(selected(page)).toHaveCount(0);
	await expect(page.getByText(root, { exact: true }).or(page.getByText(leaf, { exact: true }))).toHaveCount(0);
	await expect(page.getByLabel('Exact user ID')).toHaveValue('');
	const restart = page.getByRole('button', { name: 'Restart lookup' });
	if (status === 0 || status === 422) {
		await restart.press('Enter');
		await expect(selected(page)).toBeVisible();
		expect(calls).toEqual([tree(root), next, tree(root)]);
	} else {
		await expect(restart).toHaveCount(0);
		expect(calls).toEqual([tree(root), next]);
	}
});

test('rejects a successful response for a different node without displaying its IDs', async ({ page }) => {
	const calls = await mock(page, (route) => route.fulfill({ json: result(branch, root, [leaf], cursor) }));
	await page.goto(path);
	await lookup(page, root);
	await expect(page.getByRole('alert')).toContainText('Could not load the invite tree');
	await expect(selected(page)).toHaveCount(0);
	await expect(list(page)).toHaveCount(0);
	await expect(more(page)).toHaveCount(0);
	expect(calls).toEqual([tree(root)]);
});

test('clears IDs on cross-tab generation loss or page restore without replaying the lookup', async ({ page, context }) => {
	const calls = await mock(page, (route) => route.fulfill({ json: result(root, null, [leaf]) }));
	await page.goto(path);
	await lookup(page, root);
	await expect(selected(page)).toBeVisible();
	const otherTab = await context.newPage();
	await otherTab.route(`**${path}`, (route) => route.fulfill({ contentType: 'text/html', body: '' }));
	await otherTab.goto(path);
	await otherTab.evaluate((key) => localStorage.removeItem(key), generationKey);
	await expect(selected(page)).toHaveCount(0);
	await lookup(page, root);
	await expect(page.getByRole('alert')).toContainText('unavailable');
	await expect(page.getByLabel('Exact user ID')).toHaveValue('');
	await expect(selected(page)).toHaveCount(0);
	expect(calls).toEqual([tree(root)]);
	await page.evaluate((key) => localStorage.setItem(key, 'new-session'), generationKey);
	await lookup(page, root);
	await expect(selected(page)).toBeVisible();
	await page.evaluate(() => dispatchEvent(new PageTransitionEvent('pagehide', { persisted: true })));
	await expect(selected(page)).toHaveCount(0);
	await page.evaluate(() => dispatchEvent(new PageTransitionEvent('pageshow', { persisted: true })));
	await expect(page.getByLabel('Exact user ID')).toHaveValue('');
	await expect(selected(page)).toHaveCount(0);
	expect(calls).toEqual([tree(root), tree(root)]);
});
