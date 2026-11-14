import { mkdir, readFile, writeFile } from 'node:fs/promises';
import path from 'node:path';
import type { FullConfig, Reporter, Suite, TestCase } from '@playwright/test/reporter';

function titlePath(test: TestCase): string[] {
	const titles = [test.title];
	for (let suite: Suite | undefined = test.parent; suite; suite = suite.parent) {
		if (suite.type === 'describe') titles.unshift(suite.title);
	}
	return titles;
}

export default class RuntimeInventory implements Reporter {
	private config!: FullConfig;
	private suite!: Suite;

	printsToStdio(): boolean {
		return false;
	}

	onBegin(config: FullConfig, suite: Suite): void {
		this.config = config;
		this.suite = suite;
	}

	async onEnd(): Promise<void> {
		const githubSha = process.env.GITHUB_SHA ?? null;
		const eventPath = process.env.GITHUB_EVENT_PATH;
		let headSha = githubSha;
		if (eventPath) {
			const event = JSON.parse(await readFile(eventPath, 'utf8'));
			const pullRequestHead = event?.pull_request?.head?.sha;
			if (typeof pullRequestHead === 'string') headSha = pullRequestHead;
		}

		const project = this.config.projects[0];
		const tests = this.suite.allTests().map((test) => {
			const testProject = test.parent.project()!;
			return {
				file: path.relative(testProject.testDir, test.location.file).split(path.sep).join('/'),
				titlePath: titlePath(test),
				project: testProject.name,
				expectedStatus: test.expectedStatus,
				outcome: test.outcome(),
				results: test.results.map(({ retry, status }) => ({ retry, status })),
			};
		}).sort((a, b) => {
			const left = [a.file, ...a.titlePath];
			const right = [b.file, ...b.titlePath];
			for (let index = 0; index < Math.min(left.length, right.length); index += 1) {
				if (left[index] < right[index]) return -1;
				if (left[index] > right[index]) return 1;
			}
			return left.length - right.length;
		});
		const totals = { expected: 0, unexpected: 0, flaky: 0, skipped: 0 };
		for (const test of tests) totals[test.outcome] += 1;

		const inventory = {
			schema: 'nntune.e2e.runtime-inventory/1',
			GITHUB_RUN_ID: process.env.GITHUB_RUN_ID ?? null,
			GITHUB_RUN_ATTEMPT: process.env.GITHUB_RUN_ATTEMPT ?? null,
			GITHUB_JOB: process.env.GITHUB_JOB ?? null,
			GITHUB_EVENT_NAME: process.env.GITHUB_EVENT_NAME ?? null,
			GITHUB_SHA: githubSha,
			headSha,
			playwrightVersion: this.config.version,
			workers: this.config.workers,
			retries: project.retries,
			totals,
			tests,
		};
		await mkdir(project.outputDir, { recursive: true });
		await writeFile(path.join(project.outputDir, 'runtime-inventory.json'), JSON.stringify(inventory, null, 2) + '\n');
	}
}
