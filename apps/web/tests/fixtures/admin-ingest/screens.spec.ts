import { createHash } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { expect, test as base, type Page } from '@playwright/test';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import { PRIVATE_MUSIC } from '../../../../../e2e/helpers/private-music-journey.js';
import { withIsolatedViteFixture } from '../../vite-fixture.helpers.js';
import type {} from './mount.js';

const test = base.extend<{}, { ingestOrigin: string }>({
	ingestOrigin: [async ({}, use) => {
		await withIsolatedViteFixture({
			configFile: false,
			root: fileURLToPath(new URL('.', import.meta.url)),
			publicDir: false,
			plugins: [svelte({ configFile: false, compilerOptions: { runes: true } })],
			server: {
				host: '127.0.0.1',
				port: 0,
				fs: { allow: [fileURLToPath(new URL('../../../', import.meta.url))] }
			}
		}, use);
	}, { scope: 'worker' }]
});

const clips = new URL('../../../../../fixtures/synthetic-flac/admin-upload/', import.meta.url);
const queuePage = (jobs: unknown[], offset = 0) => ({ status: 200, body: { jobs, limit: 50, offset } });

async function localOnly(page: Page, origin: string) {
	await page.route('**/*', (route) => {
		const request = route.request();
		if (new URL(request.url()).origin !== origin || ['fetch', 'xhr', 'media'].includes(request.resourceType())) {
			return route.abort();
		}
		return route.continue();
	});
}

async function openScreens(
	page: Page, origin: string, role: Window['syntheticAccount'], enabled?: boolean
) {
	await localOnly(page, origin);
	await page.addInitScript(({ role, enabled }) => {
		window.syntheticAccount = role;
		window.enableIngestFixture = enabled;
	}, { role, enabled });
	await page.goto(origin);
}

test('the app has no mounted ingest page or member navigation', async ({ page, baseURL }) => {
	await localOnly(page, new URL(baseURL!).origin);
	await page.goto(PRIVATE_MUSIC.library.path);
	await expect(page.locator('html')).toHaveAttribute('data-hydrated', '');
	await expect(page.getByRole('link', { name: 'Catalogue ingest' })).toHaveCount(0);
	await expect(page.locator('a[href^="/admin/ingest"]')).toHaveCount(0);
	expect((await page.goto('/admin/ingest'))?.status()).toBe(404);
	await expect(page.getByRole('heading', { name: /Page not found/ })).toBeVisible();
});

for (const [role, enabled] of [
	['owner', undefined], ['owner', false], ['member', true], ['anonymous', true]
] as const) {
	test(`ingest stays hidden for ${role} with fixture flag ${enabled}`, async ({ page, ingestOrigin }) => {
		let requests = 0;
		await page.exposeFunction('queueFixture', () => { requests++; return queuePage([]); });
		await openScreens(page, ingestOrigin, role, enabled);
		await expect(page.getByRole('heading', { name: 'Page not found' })).toBeVisible();
		await page.evaluate(async () => {
			document.documentElement.dataset.fixtureDocument = 'original';
			window.dispatchEvent(new PageTransitionEvent('pagehide', { persisted: true }));
			window.dispatchEvent(new PageTransitionEvent('pageshow', { persisted: true }));
			await new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
		});
		await expect(page.locator('html')).toHaveAttribute('data-fixture-document', 'original');
		await expect(page.getByRole('heading', { name: 'Page not found' })).toBeVisible();
		await expect(page.getByRole('navigation', { name: 'Catalogue ingest' })).toHaveCount(0);
		await expect(page.getByLabel('FLAC files')).toHaveCount(0);
		expect(requests).toBe(0);
	});
}

test('ten separate FLAC inputs keep independent outcomes through a denial', async ({ page, ingestOrigin }) => {
	const { tracks }: { tracks: { file: string; bytes: number; sha256: string }[] } =
		JSON.parse(await readFile(new URL('manifest.json', clips), 'utf8'));
	expect(tracks).toHaveLength(10);
	const pending = new Map(tracks.map((clip) => [clip.sha256, clip]));
	const seen: string[] = [];
	await page.exposeFunction('queueFixture', () => queuePage([]));
	await page.exposeFunction('uploadFixture', (bytes: number[]) => {
		const digest = createHash('sha256').update(Buffer.from(bytes)).digest('hex');
		const clip = pending.get(digest);
		if (!clip || clip.bytes !== bytes.length) throw new Error('Unexpected fixture FLAC input');
		pending.delete(digest);
		seen.push(clip.file);
		return clip.file === 'fixture-5.flac' ? 'denied' : 'accepted';
	});
	await openScreens(page, ingestOrigin, 'owner', true);
	await expect(page.getByRole('navigation', { name: 'Catalogue ingest' })).toBeVisible();
	await page.getByLabel('FLAC files').setInputFiles(tracks.map(({ file }) => fileURLToPath(new URL(file, clips))));
	await page.getByRole('button', { name: 'Upload files' }).click();
	await expect(page.getByText('10 of 10 files processed.')).toBeVisible();
	const rows = page.getByRole('list', { name: 'Upload results' }).getByRole('listitem');
	await expect(rows).toHaveText(tracks.map(({ file }) =>
		`${file} · ${file === 'fixture-5.flac' ? 'Upload denied for this file.' : 'Accepted for processing.'}`
	));
	expect(seen).toEqual(tracks.map(({ file }) => file));
	expect(pending.size).toBe(0);
	await expect(page.locator('video, audio, iframe')).toHaveCount(0);
});

test('the FLAC picker stays locked during a file read and accepts the next selection', async ({ page, ingestOrigin }) => {
	const firstFile = new URL('fixture-1.flac', clips);
	const nextFile = new URL('fixture-2.flac', clips);
	const expectedFiles = await Promise.all([readFile(firstFile), readFile(nextFile)]);
	const readStarted = Promise.withResolvers<void>();
	const releaseRead = Promise.withResolvers<void>();
	const received: Buffer[] = [];
	await page.exposeFunction('queueFixture', () => queuePage([]));
	await page.exposeFunction('uploadFixture', (bytes: number[]) => {
		received.push(Buffer.from(bytes));
		return 'accepted';
	});
	await page.exposeFunction('holdFirstFileRead', () => {
		readStarted.resolve();
		return releaseRead.promise;
	});
	await page.addInitScript(() => {
		const arrayBuffer = File.prototype.arrayBuffer;
		File.prototype.arrayBuffer = async function (this: File) {
			if (this.name === 'fixture-1.flac') {
				await (window as Window & { holdFirstFileRead(): Promise<void> }).holdFirstFileRead();
			}
			return arrayBuffer.call(this);
		};
	});
	await openScreens(page, ingestOrigin, 'owner', true);
	const picker = page.getByLabel('FLAC files');
	const upload = page.getByRole('button', { name: 'Upload files' });
	const results = page.getByRole('list', { name: 'Upload results' }).getByRole('listitem');
	await picker.setInputFiles(fileURLToPath(firstFile));
	await upload.click();
	await readStarted.promise;
	try {
		await expect(picker).toBeDisabled();
		await expect(upload).toBeDisabled();
		await expect(results).toHaveText(['fixture-1.flac · Uploading']);
		expect(received).toHaveLength(0);
	} finally {
		releaseRead.resolve();
	}

	await expect(results).toHaveText(['fixture-1.flac · Accepted for processing.']);
	await expect(picker).toBeEnabled();
	await expect(picker).toHaveValue('');
	expect(received).toEqual([expectedFiles[0]]);

	await picker.setInputFiles(fileURLToPath(nextFile));
	await upload.click();
	await expect(results).toHaveText(['fixture-2.flac · Accepted for processing.']);
	expect(received).toEqual(expectedFiles);
});

test('queue status filters, paging, and detail expose supported states and fixed reasons', async ({ page, ingestOrigin }) => {
	const rows = [
		{ job_id: 'accepted-job', status: 'accepted', revision: 0 },
		{ job_id: 'failed-job', status: 'failed', revision: 2, reason: 'fixture-private-diagnostic' },
		{ job_id: 'review-job', status: 'needs_review', revision: 4, reason: 'position_mismatch' },
		{ job_id: 'published-job', status: 'published', revision: 1 }
	];
	const full = rows.concat(Array.from({ length: 46 }, (_, i) =>
		({ job_id: `queued-${i}`, status: 'accepted', revision: 0 })
	));
	await page.exposeFunction('queueFixture', (query: { status?: string; limit: number; offset: number }) => {
		expect(query.limit).toBe(50);
		if (query.status) return queuePage(rows.filter(({ status }) => status === query.status));
		return query.offset ? queuePage([], query.offset) : queuePage(full);
	});
	await page.exposeFunction('detailFixture', (jobId: string) => ({
		status: 200,
		body: { ...rows.find(({ job_id }) => job_id === jobId), private_field: 'fixture-private-diagnostic' }
	}));
	await openScreens(page, ingestOrigin, 'owner', true);
	const queue = page.getByRole('list', { name: 'Review queue' });
	await expect(queue.getByRole('listitem')).toHaveCount(50);
	await page.getByRole('button', { name: 'Next page' }).click();
	await expect(page.getByText('0 jobs on this page.')).toBeVisible();
	await page.getByRole('button', { name: 'Previous page' }).click();
	await expect(queue.getByRole('listitem')).toHaveCount(50);
	for (const row of rows) {
		await page.getByRole('button', { name: `Open job ${row.job_id}` }).click();
		await expect(page.getByRole('heading', { name: `Job ${row.job_id}` })).toBeFocused();
		await expect(page.getByRole('button', { name: 'Retry match' })).toHaveCount(row.status === 'needs_review' ? 1 : 0);
		if (row.status === 'failed') await expect(page.getByText('Processing failed.', { exact: false })).toBeVisible();
		if (row.status === 'needs_review') await expect(page.getByText('File positions do not match the release track positions.')).toBeVisible();
	}
	await expect(page.getByText('fixture-private-diagnostic')).toHaveCount(0);
	await page.getByLabel('Status', { exact: true }).selectOption('needs_review');
	await page.getByRole('button', { name: 'Refresh queue' }).click();
	await expect(queue.getByRole('listitem')).toHaveText(['Open job review-job Needs review']);
	await expect(page.getByRole('button', { name: /delete|manual match/i })).toHaveCount(0);
});

test('retry keeps the revision on uncertainty, advances on a confirmed result, and clears on conflict', async ({ page, ingestOrigin }) => {
	const review = { job_id: 'review-job', status: 'needs_review', revision: 4 };
	const results = [
		{ status: 503, body: { message: 'fixture-private-diagnostic' } },
		{ status: 200, body: { ...review, revision: 5, reason: 'count_mismatch' } },
		{ status: 409, body: { message: 'fixture-private-diagnostic' } },
		{ status: 200, body: { ...review, revision: 6, status: 'published' } }
	];
	let current = review;
	await page.exposeFunction('queueFixture', () => queuePage([current]));
	await page.exposeFunction('detailFixture', () => ({ status: 200, body: current }));
	const revisions: number[] = [];
	await page.exposeFunction('retryFixture', (id: string, body: { revision: number }) => {
		expect(id).toBe(review.job_id);
		expect(Object.keys(body)).toEqual(['revision']);
		revisions.push(body.revision);
		const result = results.shift()!;
		if (result.status === 200) current = result.body as typeof current;
		return result;
	});
	await openScreens(page, ingestOrigin, 'owner', true);
	const detail = page.getByRole('heading', { name: 'Job review-job' });
	const open = page.getByRole('button', { name: 'Open job review-job' });
	const retry = page.getByRole('button', { name: 'Retry match' });
	await open.click();
	await retry.click();
	await expect(page.getByRole('alert')).toContainText('Retry again to check the same attempt.');
	await retry.click();
	await expect(page.getByText('Retry completed. The job still needs review.')).toBeVisible();
	await expect(page.getByText('The number of files differs from the release track count.')).toBeVisible();
	await retry.click();
	await expect(page.getByRole('alert')).toContainText('Refresh the queue and reopen the job.');
	await expect(detail).toHaveCount(0);
	await expect(retry).toHaveCount(0);
	await page.getByRole('button', { name: 'Refresh queue' }).click();
	await open.click();
	await retry.click();
	await expect(page.getByText('Job published.')).toBeVisible();
	await expect(retry).toHaveCount(0);
	await expect(page.getByText('fixture-private-diagnostic')).toHaveCount(0);
	expect(revisions).toEqual([4, 4, 5, 5]);
});

test('an opaque review denial clears previous queue data and actions', async ({ page, ingestOrigin }) => {
	await page.exposeFunction('queueFixture', () => queuePage([{ job_id: 'review-job', status: 'needs_review', revision: 4 }]));
	await page.exposeFunction('detailFixture', () => ({ status: 404, body: { reason: 'fixture-private-diagnostic' } }));
	await openScreens(page, ingestOrigin, 'owner', true);
	await page.getByRole('button', { name: 'Open job review-job' }).click();
	await expect(page.getByRole('alert')).toContainText('Review is unavailable.');
	await expect(page.getByRole('list', { name: 'Review queue' })).toHaveCount(0);
	await expect(page.getByRole('button', { name: 'Retry match' })).toHaveCount(0);
	await expect(page.getByText('fixture-private-diagnostic')).toHaveCount(0);
});

const pendingReview = { job_id: 'pending-review', status: 'needs_review', revision: 4 };

test('invalid queue responses cannot render a review page', async ({ page, ingestOrigin }) => {
	const replies = [
		queuePage([pendingReview]),
		queuePage([pendingReview, pendingReview]),
		queuePage([{ ...pendingReview, status: 'accepted' }]),
		queuePage([pendingReview], 50),
		{ status: 200, body: { jobs: [pendingReview], limit: 49, offset: 0 } },
		queuePage(Array.from({ length: 51 }, (_, i) => ({ ...pendingReview, job_id: `job-${i}` }))),
		queuePage([pendingReview])
	];
	await page.exposeFunction('queueFixture', () => replies.shift());
	await openScreens(page, ingestOrigin, 'owner', true);
	const queue = page.getByRole('list', { name: 'Review queue' });
	await expect(queue).toBeVisible();
	await page.getByLabel('Status', { exact: true }).selectOption('needs_review');
	for (let remaining = 5; remaining >= 1; remaining--) {
		await page.getByRole('button', { name: 'Refresh queue' }).click();
		await expect(page.getByRole('alert')).toHaveText('Review could not be loaded. Try again.');
		await expect(queue).toHaveCount(0);
		expect(replies).toHaveLength(remaining);
	}
	await page.getByRole('button', { name: 'Refresh queue' }).click();
	await expect(queue).toBeVisible();
	expect(replies).toHaveLength(0);
});

test('detail and retry responses must match the requested job and revision', async ({ page, ingestOrigin }) => {
	const foreign = 'wrong-review-job';
	const published = { ...pendingReview, status: 'published', revision: 5 };
	const replies = [
		{ ...published, job_id: foreign },
		{ ...published, revision: 6 },
		{ ...published, status: 'failed' },
		published
	];
	let detailId = foreign;
	await page.exposeFunction('queueFixture', () => queuePage([pendingReview]));
	await page.exposeFunction('detailFixture', () => ({ status: 200, body: { ...pendingReview, job_id: detailId } }));
	const revisions: number[] = [];
	await page.exposeFunction('retryFixture', (_id: string, { revision }: { revision: number }) => {
		revisions.push(revision);
		return { status: 200, body: replies.shift() };
	});
	await openScreens(page, ingestOrigin, 'owner', true);
	const open = page.getByRole('button', { name: 'Open job pending-review' });
	const retry = page.getByRole('button', { name: 'Retry match' });
	await open.click();
	await expect(page.getByRole('alert')).toHaveText('Review could not be loaded. Try again.');
	await expect(page.getByText(foreign, { exact: false })).toHaveCount(0);
	await expect(retry).toHaveCount(0);

	detailId = pendingReview.job_id;
	await open.click();
	for (let attempt = 1; attempt <= 3; attempt++) {
		await retry.click();
		await expect(page.getByRole('alert')).toContainText('Retry could not be confirmed.');
		await expect(page.getByText('Job published.')).toHaveCount(0);
		await expect(page.getByText(foreign, { exact: false })).toHaveCount(0);
		expect(revisions).toEqual(Array(attempt).fill(4));
	}
	await retry.click();
	await expect(page.getByText('Job published.')).toBeVisible();
	expect(revisions).toEqual([4, 4, 4, 4]);
});

test('a page hidden during detail aborts the old request and cached restore reloads', async ({ page, ingestOrigin }) => {
	const started = Promise.withResolvers<void>();
	const detail = Promise.withResolvers<{ status: number; body: typeof pendingReview }>();
	let queueReads = 0;
	await page.exposeFunction('queueFixture', () => {
		queueReads++;
		return queuePage(queueReads === 1 ? [pendingReview] : []);
	});
	await page.exposeFunction('detailFixture', () => {
		started.resolve();
		return detail.promise;
	});
	await openScreens(page, ingestOrigin, 'owner', true);
	await page.getByRole('button', { name: 'Open job pending-review' }).click();
	await started.promise;
	await page.evaluate(() => window.dispatchEvent(new PageTransitionEvent('pagehide', { persisted: true })));
	await expect(page.getByRole('heading', { name: 'Page not found' })).toBeVisible();
	detail.resolve({ status: 200, body: pendingReview });
	await expect.poll(() => page.evaluate(() => window.detailAbortedAtReply)).toBe(true);
	await expect(page.getByText('pending-review', { exact: false })).toHaveCount(0);

	await page.evaluate(() => {
		document.documentElement.dataset.fixtureDocument = 'original';
		window.dispatchEvent(new PageTransitionEvent('pageshow', { persisted: true }));
	});
	await expect(page.getByRole('heading', { name: 'Catalogue ingest' })).toBeVisible();
	await expect(page.locator('html')).not.toHaveAttribute('data-fixture-document', 'original');
	await expect(page.getByText('0 jobs on this page.')).toBeVisible();
	await expect(page.getByText('pending-review', { exact: false })).toHaveCount(0);
	expect(queueReads).toBe(2);
});
