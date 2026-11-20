import type { Page, Request } from '@playwright/test';

export const contextPath = '**/v1/plays/events/context';
export const eventsPath = '**/v1/plays/events';

export function isListenContextRequest(request: Request, baseURL: string | undefined): boolean {
	return !!baseURL && request.method() === 'POST' &&
		request.url() === new URL('/v1/plays/events/context', baseURL).href;
}

export function watchListenContext(page: Page) {
	return page.addInitScript(() => {
		const fetchOriginal = window.fetch.bind(window);
		window.fetch = async (input, init) => {
			const response = await fetchOriginal(input, init);
			if (String(input).endsWith('/v1/plays/events/context')) {
				const parse = response.json.bind(response);
				response.json = async () => {
					const body = await parse();
					document.documentElement.dataset.historyContextRead = 'true';
					return body;
				};
			}
			return response;
		};
	});
}
