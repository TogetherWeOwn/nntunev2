import { expect, test } from '@playwright/test';

const routes = ['/', '/login', '/signup', '/pricing', '/auth/magic'];

const requiredHeaders = {
	'strict-transport-security': 'max-age=31536000; includeSubDomains',
	'x-content-type-options': 'nosniff',
	'x-frame-options': 'DENY',
	'referrer-policy': 'no-referrer'
};

test('public pages send the expected security headers without CSP violations', async ({ page }) => {
	await page.addInitScript(() => {
		window.addEventListener('securitypolicyviolation', (event) => {
			const violations = (window as Window & { __cspViolations?: string[] }).__cspViolations ??= [];
			violations.push(`${event.violatedDirective}: ${event.blockedURI}`);
		});
	});

	for (const route of routes) {
		const response = await page.goto(route);
		expect(response, `${route} should return a document`).not.toBeNull();
		const headers = response!.headers();

		for (const [name, value] of Object.entries(requiredHeaders)) {
			expect(headers[name], `${route} should include ${name}`).toBe(value);
		}

		const permissionsPolicy = headers['permissions-policy'];
		expect(permissionsPolicy, `${route} should include permissions-policy`).toBeTruthy();
		expect(permissionsPolicy).toContain('camera=()');
		expect(permissionsPolicy).toContain('microphone=()');
		expect(permissionsPolicy).toContain('geolocation=()');
		expect(permissionsPolicy).not.toMatch(/publickey-credentials-(?:get|create)/);

		const csp = headers['content-security-policy'];
		expect(csp, `${route} should include content-security-policy`).toBeTruthy();
		const directives = new Map(
			csp!.split(';').map((directive) => {
				const [name, ...sources] = directive.trim().split(/\s+/);
				return [name, sources];
			})
		);
		expect(directives.get('default-src')).toContain("'self'");
		expect(directives.get('object-src')).toContain("'none'");
		expect(directives.get('base-uri')).toContain("'self'");
		expect(directives.get('form-action')).toContain("'self'");
		expect(directives.get('frame-ancestors')).toContain("'none'");
		expect(directives.get('script-src')).toBeDefined();
		expect(directives.get('script-src')).not.toContain("'unsafe-inline'");
		expect(directives.get('script-src')).not.toContain("'unsafe-eval'");

		expect(
			(await page.evaluate(() => (window as Window & { __cspViolations?: string[] }).__cspViolations ?? [])),
			`${route} should not cause a CSP violation`
		).toEqual([]);
	}
});
