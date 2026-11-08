import { describe, expect, it } from 'vitest';
import { render } from 'svelte/server';
import SettingsPage from './+page.svelte';

describe('settings ListenBrainz section (pre-PUT, server render)', () => {
	const { body } = render(SettingsPage);

	it('sits after Your Library and before Account', () => {
		const library = body.indexOf('settings-library-heading');
		const listenbrainz = body.indexOf('settings-listenbrainz-heading');
		const account = body.indexOf('settings-account-heading');
		expect(library).toBeGreaterThan(-1);
		expect(listenbrainz).toBeGreaterThan(library);
		expect(account).toBeGreaterThan(listenbrainz);
		expect(body).toMatch(/<h2[^>]*id="settings-listenbrainz-heading"[^>]*>ListenBrainz<\/h2>/);
	});

	it('renders no link or unlink control before status is known', () => {
		expect(body).not.toMatch(/>\s*Unlink\s*</);
		expect(body).not.toContain('Retry');
		expect(body).not.toContain('Linked as');
	});

	it('has no token input, link form or Change account control', () => {
		expect(body).not.toMatch(/type="password"/);
		expect(body).not.toMatch(/Change account|Link account|Save new account|User token/i);
		expect(body).not.toMatch(/<form/);
	});

	it('never implies delivery of listens or feedback', () => {
		expect(body).not.toMatch(/syncing|scrobbl|being shared/i);
	});
});
