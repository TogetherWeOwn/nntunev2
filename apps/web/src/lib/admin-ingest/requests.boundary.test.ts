import { expect, test } from 'vitest';
import { MAX_REVISION, readJob, readQueue } from './requests.js';

const job = { job_id: 'review-1', revision: 4, status: 'needs_review' } as const;
const query = { limit: 50, offset: 0, status: 'needs_review' } as const;
const page = { jobs: [job], limit: query.limit, offset: query.offset };

test('accepts only dense, bounded pages with their own fields', () => {
	expect(readQueue(page, query).jobs).toHaveLength(1);
	for (const fields of [{ limit: 101 }, { limit: 0 }, { offset: -1 }, { offset: MAX_REVISION + 1 }]) {
		const invalid = { ...page, ...fields, jobs: [] };
		expect(() => readQueue(invalid, { ...query, ...fields })).toThrow();
	}
	expect(() => readQueue(Object.create(page), query)).toThrow();
	expect(() => readQueue({ ...page, jobs: Array(1) }, query)).toThrow();
	expect(() => readJob(Object.create(job))).toThrow();
});
