<script lang="ts">
	import { onMount, tick } from 'svelte';
	import {
		MAX_REVISION, PAGE_SIZE, STATUSES, readDetail, readJob, readQueue, uploadMessage,
		type IngestRequests, type ReviewQueue, type ReviewStatus
	} from './requests.js';

	let { requests }: { requests: IngestRequests } = $props();
	const lifetime = new AbortController();
	const { signal } = lifetime;
	let picker: HTMLInputElement;
	let reviewHeading: HTMLHeadingElement;
	let detailHeading = $state<HTMLHeadingElement>();
	let uploading = $state(false);
	let uploads = $state<{ name: string; message: string }[]>([]);
	let completed = $state(0);
	let busy = $state(false);
	let filter = $state<ReviewStatus | ''>('');
	let appliedFilter = $state<ReviewStatus | ''>('');
	let queue = $state.raw<ReviewQueue>();
	let job = $state.raw<ReturnType<typeof readJob>>();
	let failure = $state('');
	let notice = $state('');

	onMount(() => {
		void loadQueue();
		return () => lifetime.abort();
	});

	async function upload(event: SubmitEvent) {
		event.preventDefault();
		if (uploading || signal.aborted) return;
		const files = Array.from(picker.files ?? []);
		if (files.length === 0) return;
		picker.value = '';
		uploads = files.map(({ name }) => ({ name, message: 'Waiting' }));
		completed = 0;
		uploading = true;
		for (const [index, file] of files.entries()) {
			uploads[index].message = 'Uploading';
			let outcome: unknown;
			try {
				outcome = file.size && /\.flac$/i.test(file.name)
					? await requests.upload(file, signal) : 'invalid';
			} catch {
				// The next file has an independent outcome.
			}
			if (signal.aborted) return;
			uploads[index].message = uploadMessage(outcome);
			completed++;
		}
		uploading = false;
	}

	function reviewFailure(status?: number) {
		if (status === 404 || status === 401 || status === 403) {
			queue = job = undefined;
			failure = 'Review is unavailable. Refresh the queue to check access.';
		} else {
			failure = 'Review could not be loaded. Try again.';
		}
	}

	async function loadQueue(offset = 0) {
		if (busy || signal.aborted) return;
		busy = true;
		queue = job = undefined;
		failure = notice = '';
		const query = { status: filter || undefined, limit: PAGE_SIZE, offset };
		appliedFilter = filter;
		try {
			const result = await requests.queue(query, signal);
			if (signal.aborted) return;
			if (result.status === 200) queue = readQueue(result.body, query);
			else reviewFailure(result.status);
		} catch {
			if (!signal.aborted) reviewFailure();
		} finally {
			busy = false;
		}
	}

	function pageBy(delta: number) {
		if (!queue || filter !== appliedFilter) return;
		reviewHeading.focus();
		void loadQueue(Math.max(0, queue.offset + delta));
	}

	async function openJob(id: string) {
		if (busy || signal.aborted) return;
		const listed = queue?.jobs.find((row) => row.job_id === id);
		if (!listed) return;
		reviewHeading.focus();
		busy = true;
		job = undefined;
		failure = notice = '';
		try {
			const result = await requests.detail(id, signal);
			if (signal.aborted) return;
			if (result.status !== 200) return reviewFailure(result.status);
			job = readDetail(result.body, listed);
			await tick();
			if (!signal.aborted) detailHeading?.focus();
		} catch {
			if (!signal.aborted) reviewFailure();
		} finally {
			busy = false;
		}
	}

	async function retry() {
		const current = job;
		if (busy || signal.aborted || current?.status !== 'needs_review' || current.revision === MAX_REVISION) return;
		detailHeading?.focus();
		busy = true;
		failure = notice = '';
		try {
			const result = await requests.retry(current.job_id, { revision: current.revision }, signal);
			if (signal.aborted) return;
			if (result.status === 409) {
				reviewHeading.focus();
				queue = job = undefined;
				failure = 'This job changed or its file could not be verified. Refresh the queue and reopen the job.';
				return;
			}
			if ([401, 403, 404].includes(result.status)) {
				reviewHeading.focus();
				return reviewFailure(result.status);
			}
			if (result.status !== 200) throw new Error('Retry outcome unavailable');
			const updated = readJob(result.body);
			if (updated.job_id !== current.job_id || updated.revision !== current.revision + 1 ||
				(updated.status !== 'needs_review' && updated.status !== 'published')) {
				throw new Error('Unexpected retry result');
			}
			job = updated;
			queue = undefined;
			notice = updated.status === 'published' ? 'Job published.' : 'Retry completed. The job still needs review.';
		} catch {
			if (!signal.aborted) failure = 'Retry could not be confirmed. Retry again to check the same attempt.';
		} finally {
			busy = false;
		}
	}
</script>

<main class="nt-wrap">
	<h1>Catalogue ingest</h1>
	<nav aria-label="Catalogue ingest">
		<a href="#flac-upload">Upload FLAC</a>
		<a href="#ingest-review">Review jobs</a>
	</nav>

	<section aria-labelledby="flac-upload">
		<h2 id="flac-upload">Upload FLAC</h2>
		<form onsubmit={upload}>
			<label for="flac-files">FLAC files</label>
			<input id="flac-files" type="file" accept=".flac,audio/flac,audio/x-flac"
				multiple required bind:this={picker} disabled={uploading} />
			<button class="nt-button" type="submit" disabled={uploading}>Upload files</button>
		</form>
		<p role="status">{uploads.length ? `${completed} of ${uploads.length} files processed.` : ''}</p>
		{#if uploads.length}
			<ul aria-label="Upload results">
				{#each uploads as { name, message }}
					<li><bdi>{name}</bdi> · {message}</li>
				{/each}
			</ul>
		{/if}
	</section>

	<section aria-labelledby="ingest-review">
		<h2 id="ingest-review" tabindex="-1" bind:this={reviewHeading}>Review jobs</h2>
		<form onsubmit={(event) => { event.preventDefault(); void loadQueue(); }}>
			<label for="review-filter">Status</label>
			<select id="review-filter" bind:value={filter} disabled={busy}>
				<option value="">All statuses</option>
				{#each Object.entries(STATUSES) as [status, label]}
					<option value={status}>{label}</option>
				{/each}
			</select>
			<button class="nt-button" type="submit" disabled={busy}>Refresh queue</button>
		</form>
		<p role="alert">{failure}</p>
		<p role="status">{notice || (queue ? `${queue.jobs.length} jobs on this page.` : '')}</p>
		{#if busy}<p>Review request in progress…</p>{/if}
		{#if queue}
			<ul aria-label="Review queue">
				{#each queue.jobs as row (row.job_id)}
					<li>
						<button class="nt-button nt-button--ghost" disabled={busy} onclick={() => openJob(row.job_id)}>
							Open job {row.job_id}
						</button>
						{STATUSES[row.status]}
					</li>
				{/each}
			</ul>
			{#if queue.offset > 0}
				<button class="nt-button" disabled={busy || filter !== appliedFilter} onclick={() => pageBy(-PAGE_SIZE)}>
					Previous page
				</button>
			{/if}
			{#if queue.jobs.length === queue.limit && queue.offset + queue.limit <= MAX_REVISION}
				<button class="nt-button" disabled={busy || filter !== appliedFilter} onclick={() => pageBy(PAGE_SIZE)}>Next page</button>
			{/if}
		{/if}
		{#if job}
			<h3 tabindex="-1" bind:this={detailHeading}>Job {job.job_id}</h3>
			<p>{STATUSES[job.status]}</p>
			{#if job.explanation}<p>{job.explanation}</p>
			{:else if job.status === 'failed'}<p>Processing failed. No further details are available.</p>{/if}
			{#if job.status === 'needs_review' && job.revision < MAX_REVISION}
				<p id="retry-hint">A verified match is published automatically.</p>
				<button class="nt-button" aria-describedby="retry-hint" disabled={busy} onclick={retry}>Retry match</button>
			{/if}
		{/if}
	</section>
</main>

<style>
	nav, form { display: flex; flex-wrap: wrap; align-items: center; gap: var(--nt-space-3); }
	section { margin-block: var(--nt-space-5); }
	input, select, button { min-height: 44px; font: inherit; }
	input { max-width: 100%; }
	select { background: var(--nt-bg); color: var(--nt-fg); }
	li, h3 { overflow-wrap: anywhere; }
	li + li { margin-top: var(--nt-space-2); }
	p:empty { margin: 0; }
</style>
