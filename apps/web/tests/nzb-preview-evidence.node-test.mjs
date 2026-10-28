import { strict as assert } from 'node:assert';
import { execFileSync } from 'node:child_process';
import { test } from 'node:test';
import { previewSourceMetadata } from './nzb-preview-evidence.mjs';

const helper = new URL('./nzb-preview-evidence.mjs', import.meta.url).href;

test('preview evidence records real source metadata in an ownership-mismatched container', () => {
	const expected = previewSourceMetadata();
	const actual = JSON.parse(execFileSync(process.execPath, ['--input-type=module', '-e',
		`import { previewSourceMetadata } from ${JSON.stringify(helper)}; process.stdout.write(JSON.stringify(previewSourceMetadata()));`
	], {
		encoding: 'utf8',
		env: { ...process.env, GIT_TEST_ASSUME_DIFFERENT_OWNER: '1', GIT_CONFIG_GLOBAL: '/dev/null', GIT_CONFIG_SYSTEM: '/dev/null' }
	}));
	assert.deepEqual(actual, expected);
	assert.match(actual.head_sha, /^[a-f0-9]{40}$/);
	assert.equal(typeof actual.source_tree_dirty, 'boolean');
});
