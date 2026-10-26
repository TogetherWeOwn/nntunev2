import { createServer } from 'node:http';
import { readFile, readdir } from 'node:fs/promises';
import { extname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

export async function createProbeServer(distUrl) {
	const types = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css' };
	const assets = new Map();
	// Snapshot regular build outputs only. HTTP input never selects a disk path,
	// and symlinks cannot expose sources or other files outside the build.
	async function load(directory, prefix = '') {
		for (const entry of await readdir(directory, { withFileTypes: true })) {
			const filePath = join(directory, entry.name);
			const key = `${prefix}/${entry.name}`;
			if (entry.isDirectory()) await load(filePath, key);
			else if (entry.isFile()) assets.set(key, {
				body: await readFile(filePath),
				type: types[extname(entry.name)] ?? 'application/octet-stream'
			});
		}
	}
	await load(fileURLToPath(distUrl));
	return createServer((req, res) => {
		const path = (req.url ?? '').split('?')[0];
		const asset = assets.get(path === '/' ? '/index.html' : path);
		if (!asset) {
			res.writeHead(404).end();
			return;
		}
		res.writeHead(200, { 'content-type': asset.type });
		res.end(asset.body);
	});
}
