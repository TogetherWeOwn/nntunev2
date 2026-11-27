export const STATUSES = {
	accepted: 'Accepted',
	failed: 'Failed',
	needs_review: 'Needs review',
	published: 'Published'
} as const;

const REASONS = {
	invalid_query: 'The release search could not be formed.',
	search_evidence: 'The release search did not produce usable evidence.',
	release_unresolved: 'A release could not be identified.',
	recording_evidence: 'The recording matches could not be verified.',
	count_mismatch: 'The number of files differs from the release track count.',
	file_numbering: 'The file numbering could not be verified.',
	position_mismatch: 'File positions do not match the release track positions.'
} as const;

export type ReviewStatus = keyof typeof STATUSES;
type Job = { job_id: string; revision: number; status: ReviewStatus; explanation?: string };
export type ReviewQueue = { jobs: Job[]; limit: number; offset: number };
type QueueRequest = { status?: ReviewStatus; limit: number; offset: number };
type ReviewResponse = { status: number; body?: unknown };
type UploadOutcome = 'accepted' | 'denied' | 'invalid';

// The app proxy must authorize each operation. Upload outcomes describe UI
// states only. The adapter owns transport, authentication and step-up.
export type IngestRequests = {
	upload(file: File, signal: AbortSignal): Promise<UploadOutcome>;
	queue(query: QueueRequest, signal: AbortSignal): Promise<ReviewResponse>;
	detail(jobId: string, signal: AbortSignal): Promise<ReviewResponse>;
	retry(jobId: string, body: { revision: number }, signal: AbortSignal): Promise<ReviewResponse>;
};

export const MAX_REVISION = 0xffff_ffff;
export const PAGE_SIZE = 50;

function record(value: unknown): Record<string, unknown> | undefined {
	if (value && typeof value === 'object' && !Array.isArray(value)) return value as Record<string, unknown>;
}

function u32(value: unknown): value is number {
	return typeof value === 'number' && Number.isInteger(value) && value >= 0 && value <= MAX_REVISION;
}

function isStatus(value: unknown): value is ReviewStatus {
	return typeof value === 'string' && Object.hasOwn(STATUSES, value);
}

export function readJob(value: unknown): Job {
	const row = record(value) ?? {};
	const { job_id, revision, status, reason } = row;
	if (typeof job_id !== 'string' || !/^[A-Za-z0-9_-]{1,64}$/.test(job_id) ||
		!u32(revision) || !isStatus(status) ||
		!['job_id', 'revision', 'status'].every((key) => Object.hasOwn(row, key))) {
		throw new Error('Invalid review job');
	}
	const explanation = Object.hasOwn(row, 'reason') && typeof reason === 'string' && Object.hasOwn(REASONS, reason)
		? REASONS[reason as keyof typeof REASONS]
		: undefined;
	return { job_id, revision, status, explanation };
}

export function readQueue(value: unknown, query: QueueRequest): ReviewQueue {
	const page = record(value) ?? {};
	const { jobs, limit, offset } = page;
	if (!u32(limit) || limit === 0 || limit > 100 || !u32(offset) ||
		limit !== query.limit || offset !== query.offset || !Array.isArray(jobs) || jobs.length > limit ||
		(query.status !== undefined && !isStatus(query.status)) ||
		!['jobs', 'limit', 'offset'].every((key) => Object.hasOwn(page, key))) {
		throw new Error('Invalid review page');
	}
	const rows = Array.from({ length: jobs.length }, (_, index) => {
		if (!Object.hasOwn(jobs, index)) throw new Error('Missing review job');
		return readJob(jobs[index]);
	});
	if (new Set(rows.map((job) => job.job_id)).size !== rows.length ||
		rows.some((job) => query.status && job.status !== query.status)) {
		throw new Error('Invalid review queue');
	}
	return { jobs: rows, limit, offset };
}

export function readDetail(value: unknown, listed: Job): Job {
	const detail = readJob(value);
	if (detail.job_id !== listed.job_id || detail.revision < listed.revision ||
		(detail.revision === listed.revision && detail.status !== listed.status)) {
		throw new Error('Unexpected review job');
	}
	return detail;
}

export function uploadMessage(outcome: unknown): string {
	switch (outcome) {
		case 'accepted': return 'Accepted for processing.';
		case 'denied': return 'Upload denied for this file.';
		case 'invalid': return 'A complete FLAC file is required.';
		default: return 'Upload could not be confirmed. Check the review queue before submitting again.';
	}
}
