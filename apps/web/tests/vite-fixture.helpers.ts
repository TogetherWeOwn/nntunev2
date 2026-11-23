import { mkdtemp, rm } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { createServer, type InlineConfig } from 'vite';

export async function withIsolatedViteFixture(config: InlineConfig, use: (origin: string) => Promise<void>) {
	// Keep optimized dependencies within the web root's fs.allow, but do not
	// share optimizer metadata between fixture servers or Playwright workers.
	const cacheDir = await mkdtemp(fileURLToPath(new URL('../node_modules/.vite-fixture-', import.meta.url)));
	try {
		const server = await createServer({ ...config, cacheDir });
		try {
			await server.listen();
			const address = server.httpServer?.address();
			if (!address || typeof address === 'string') throw new Error('no fixture TCP address');
			await use(`http://127.0.0.1:${address.port}`);
		} finally {
			await server.close();
		}
	} finally {
		await rm(cacheDir, { recursive: true, force: true });
	}
}
