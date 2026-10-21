import type { RequestHandler } from './$types';

const REPORTING_URL = 'https://github.com/TogetherWeOwn/nntunev2/security/advisories/new';
const EXPIRY_WINDOW_MS = 365 * 24 * 60 * 60 * 1000;

export const GET: RequestHandler = () => {
	const expires = new Date(Date.now() + EXPIRY_WINDOW_MS).toISOString();
	const body = [
		`Contact: ${REPORTING_URL}`,
		`Expires: ${expires}`,
		''
	].join('\n');

	return new Response(body, {
		status: 200,
		headers: { 'content-type': 'text/plain; charset=utf-8' }
	});
};
