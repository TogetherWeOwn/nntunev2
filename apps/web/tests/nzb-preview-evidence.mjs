import { execFileSync } from 'node:child_process';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const repository = resolve(fileURLToPath(new URL('../../../', import.meta.url)));

export function previewSourceMetadata() {
	// Trust only this checkout, for these two read-only invocations. No global config.
	const git = (...args) => execFileSync('git', ['-c', `safe.directory=${repository}`, ...args], { cwd: repository, encoding: 'utf8' }).trim();
	return {
		head_sha: git('rev-parse', 'HEAD'),
		source_tree_dirty: git('status', '--porcelain').length > 0
	};
}
