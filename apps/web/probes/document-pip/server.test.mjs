import assert from 'node:assert/strict';
import { mkdir, mkdtemp, rm, symlink, writeFile } from 'node:fs/promises';
import { request } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { pathToFileURL } from 'node:url';
import { createProbeServer } from './server.mjs';

async function fixture(t, directory = 'dist') {
	const root = await mkdtemp(join(tmpdir(), 'document-pip-server-'));
	t.after(() => rm(root, { recursive: true, force: true }));
	const dist = join(root, directory);
	await mkdir(join(dist, 'assets'), { recursive: true });
	await writeFile(join(dist, 'index.html'), '<p>probe</p>');
	await writeFile(join(dist, 'assets', 'probe.css'), 'p { color: red; }');
	await writeFile(join(root, 'outside.txt'), 'outside marker');
	await mkdir(join(root, 'dist-sibling'));
	await writeFile(join(root, 'dist-sibling', 'outside.txt'), 'sibling marker');
	await symlink('../outside.txt', join(dist, 'link.txt'));
	await symlink('../dist-sibling', join(dist, 'linked-directory'));
	return { dist, start: async () => {
		const server = await createProbeServer(pathToFileURL(dist));
		await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
		t.after(() => new Promise((resolve) => server.close(resolve)));
		return (path) => new Promise((resolve, reject) => {
			const req = request({ host: '127.0.0.1', port: server.address().port, path }, (res) => {
				let body = '';
				res.setEncoding('utf8');
				res.on('data', (chunk) => { body += chunk; });
				res.on('end', () => resolve({ status: res.statusCode, type: res.headers['content-type'], body }));
			});
			req.on('error', reject);
			req.end();
		});
	} };
}

test('serves built assets and query strings from a build directory containing spaces', async (t) => {
	const { start } = await fixture(t, 'build output');
	const get = await start();
	assert.deepEqual(await get('/'), { status: 200, type: 'text/html', body: '<p>probe</p>' });
	assert.deepEqual(await get('/assets/probe.css?v=1'), {
		status: 200, type: 'text/css', body: 'p { color: red; }'
	});
});

test('does not expose traversal paths, missing files or symlinks outside the build', async (t) => {
	const { start } = await fixture(t);
	const get = await start();
	for (const path of [
		'/../outside.txt', '/../dist-sibling/outside.txt', '/%2e%2e/outside.txt',
		'/missing.txt', '/link.txt', '/linked-directory/outside.txt'
	]) {
		assert.equal((await get(path)).status, 404, path);
	}
});

test('serves only startup snapshot bytes rather than reading request-selected disk paths', async (t) => {
	const { dist, start } = await fixture(t);
	const get = await start();
	await writeFile(join(dist, 'index.html'), 'changed after startup');
	await writeFile(join(dist, 'later.txt'), 'not in the build snapshot');
	assert.equal((await get('/')).body, '<p>probe</p>');
	assert.equal((await get('/later.txt')).status, 404);
});
