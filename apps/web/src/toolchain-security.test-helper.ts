import { isAlias, isMap, isScalar, isSeq, parseDocument, type YAMLMap } from 'yaml';

export type Toolchain = { wrangler: string; miniflare: string; undici: string };
const approvedWrangler = '4.153.0';

function requireValue(condition: unknown, message: string): asserts condition {
	if (!condition) throw new Error(message);
}

function mapping(node: unknown, label: string): YAMLMap {
	requireValue(isMap(node), `${label}: missing or unsupported mapping`);
	return node;
}

function scalar(node: unknown, label: string): string {
	requireValue(isScalar(node) && typeof node.value === 'string', `${label}: expected a literal string`);
	return node.value;
}

function document(source: string, label: string) {
	const parsed = parseDocument(source, { version: '1.2', uniqueKeys: true });
	requireValue(
		parsed.errors.length === 0 && parsed.warnings.length === 0,
		`${label}: invalid or unsupported YAML (${[...parsed.errors, ...parsed.warnings].map((error) => error.code).join(', ')})`
	);
	return mapping(parsed.contents, label);
}

function child(parent: YAMLMap, key: string, label: string) {
	return mapping(parent.get(key, true), `${label}.${key}`);
}

export function selectedToolchain(source: string): Toolchain {
	const root = document(source, 'lockfile');
	const importer = child(child(root, 'importers', 'lockfile'), '.', 'importers');
	const dependencies = child(importer, 'devDependencies', 'importer');
	const adapter = child(dependencies, '@sveltejs/adapter-cloudflare', 'devDependencies');
	const adapterVersion = scalar(adapter.get('version', true), 'adapter.version');
	const packages = child(root, 'packages', 'lockfile');
	const snapshots = child(root, 'snapshots', 'lockfile');

	function snapshot(name: string, reference: string) {
		child(packages, `${name}@${reference.split('(')[0]}`, 'packages');
		return child(snapshots, `${name}@${reference}`, 'snapshots');
	}
	function dependency(name: string, reference: string, key: string) {
		const edges = child(snapshot(name, reference), 'dependencies', name);
		return scalar(edges.get(key, true), `${name}.dependencies.${key}`);
	}

	const wrangler = dependency('@sveltejs/adapter-cloudflare', adapterVersion, 'wrangler');
	const miniflare = dependency('wrangler', wrangler, 'miniflare');
	const undici = dependency('miniflare', miniflare, 'undici');
	snapshot('undici', undici);
	return {
		wrangler: wrangler.split('(')[0],
		miniflare: miniflare.split('(')[0],
		undici: undici.split('(')[0]
	};
}

export function assertPatchedToolchain({ wrangler, miniflare, undici }: Toolchain) {
	const stable = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.test(undici);
	const [major, minor, patch] = undici.split('.').map(Number);
	requireValue(
		stable &&
			[major, minor, patch].every(Number.isSafeInteger) &&
			major === 7 &&
			(minor! > 30 || (minor === 30 && patch! >= 1)),
		`Wrangler ${wrangler} -> Miniflare ${miniflare} -> undici ${undici}: requires supported stable 7.x >=7.30.1`
	);
	requireValue(wrangler === approvedWrangler, 'selected Wrangler must remain on the approved minor');
}

function walk(node: unknown, path: string[], visit: (path: string[], value: unknown) => void) {
	requireValue(!isAlias(node), 'workflow: YAML aliases are not supported selections');
	if (isMap(node)) {
		for (const pair of node.items) {
			const key = scalar(pair.key, 'workflow mapping key');
			const next = [...path, key];
			visit(next, pair.value);
			walk(pair.value, next, visit);
		}
	} else if (isSeq(node)) {
		node.items.forEach((item, index) => walk(item, [...path, String(index)], visit));
	}
}

type Word = { text: string; expansion: boolean };

// Only tokenize shell words/operators; never execute a fixture. Quoted program
// arguments and comments remain data, not YAML keys or separate CLI consumers.
function shellCommands(script: string): Word[][] {
	const commands: Word[][] = [];
	let words: Word[] = [];
	let text = '';
	let active = false;
	let expansion = false;
	let quote = '';
	let heredocs: { delimiter: string; quoted: boolean; stripTabs: boolean }[] = [];
	function word() {
		if (active) words.push({ text, expansion });
		text = '';
		active = false;
		expansion = false;
	}
	function command() {
		word();
		if (words.length) commands.push(words);
		words = [];
	}
	for (let index = 0; index < script.length; index++) {
		const char = script[index]!;
		if (quote === "'") {
			if (char === quote) quote = '';
			else text += char;
			continue;
		}
		if (script.startsWith('${{', index)) {
			const end = script.indexOf('}}', index + 3);
			requireValue(end !== -1, 'workflow: unterminated expression');
			text += script.slice(index, end + 2);
			active = true;
			index = end + 1;
			continue;
		}
		if (char === '\\') {
			const next = script[++index];
			if (next !== '\n' && next !== undefined) {
				text += next;
				active = true;
			}
			continue;
		}
		if (char === '`' || (char === '$' && script[index + 1] === '(')) expansion = true;
		if (quote) {
			if (char === quote) quote = '';
			else text += char;
			continue;
		}
		if (script.startsWith('<<', index)) {
			const marker = script.slice(index).match(/^<<(-)?[ \t]*(['"]?)([A-Za-z_]\w*)\2(?=$|[\s;|&])/);
			requireValue(marker, 'workflow: unsupported here-document delimiter');
			word();
			heredocs.push({ delimiter: marker[3]!, quoted: Boolean(marker[2]), stripTabs: Boolean(marker[1]) });
			index += marker[0].length - 1;
			continue;
		}
		if (char === '\n' && heredocs.length) {
			command();
			let start = index + 1;
			for (const marker of heredocs) {
				let cursor = start;
				while (true) {
					const end = script.indexOf('\n', cursor);
					const line = script.slice(cursor, end === -1 ? script.length : end);
					if ((marker.stripTabs ? line.replace(/^\t+/, '') : line) === marker.delimiter) {
						const body = script.slice(start, cursor);
						requireValue(
							marker.quoted || !(/wrangler/i.test(body) && /\$\(|`/.test(body)),
							'workflow: unsupported Wrangler here-document substitution'
						);
						start = end === -1 ? script.length : end + 1;
						break;
					}
					requireValue(end !== -1, 'workflow: unterminated here-document');
					cursor = end + 1;
				}
			}
			heredocs = [];
			index = start - 1;
			continue;
		}
		if (char === "'" || char === '"') {
			quote = char;
			active = true;
		} else if (char === '#' && !active) {
			const end = script.indexOf('\n', index);
			index = end === -1 ? script.length : end - 1;
		} else if (';|&(){}<>\n'.includes(char)) {
			command();
		} else if (/\s/.test(char)) {
			word();
		} else {
			text += char;
			active = true;
		}
	}
	requireValue(!quote, 'workflow: unsupported unterminated shell quote');
	requireValue(heredocs.length === 0, 'workflow: unterminated here-document');
	command();
	return commands;
}

function cliSelections(script: string, resolve: (value: string) => void) {
	if (!/wrangler/i.test(script) && !/\bnpx\b/.test(script)) return 0;
	let count = 0;
	for (const words of shellCommands(script)) {
		let tokens = words.map((word) => word.text);
		requireValue(
			!(words.some((word) => word.expansion) && tokens.some((token) => /wrangler/i.test(token))),
			'workflow: unsupported Wrangler command substitution'
		);
		while (tokens.length && ['if', 'elif', 'then', 'else', 'do', 'while', 'until', '!', 'command', 'exec', 'time'].includes(tokens[0]!)) {
			tokens = tokens.slice(1);
		}
		while (/^[A-Za-z_]\w*=/.test(tokens[0] ?? '')) {
			requireValue(!/^WRANGLER_VERSION=/i.test(tokens[0]!), 'workflow: shell Wrangler selection override');
			tokens = tokens.slice(1);
		}
		const head = tokens[0];
		if (head === 'echo' || head === 'printf') continue;
		const mentions = tokens.some((token) => /(?:^|=|\/)wrangler(?:@|$)/i.test(token));
		if (head !== 'npx') {
			requireValue(
				!mentions &&
					!(['sh', 'bash', 'zsh', 'eval'].includes(head?.split('/').at(-1) ?? '') &&
						tokens.slice(1).some((token) => /wrangler/i.test(token))) &&
					!(['node', 'nodejs', 'bun', 'deno', 'python', 'python3', 'ruby', 'perl', 'php', 'pwsh', 'powershell'].includes(head?.split('/').at(-1) ?? '') &&
						tokens.slice(1).some((token) => /wrangler/i.test(token) &&
							(/wrangler@/i.test(token) || /exec|spawn|system|popen|subprocess|child_process/i.test(token)))),
				'workflow: unsupported Wrangler command; use an explicit pinned npx selection'
			);
			continue;
		}
		let index = 1;
		while (tokens[index] === '--yes' || tokens[index] === '-y') index++;
		const selected = tokens[index] ?? '';
		if (!mentions) {
			requireValue(!/[$`]/.test(selected), 'workflow: unsupported dynamic npx package selection');
			continue;
		}
		requireValue(selected.startsWith('wrangler@'), 'workflow: missing or unsupported Wrangler CLI pin');
		resolve(selected.slice('wrangler@'.length));
		count++;
	}
	return count;
}

export function assertWorkflowSelections(source: string, expected: string): number {
	requireValue(expected === approvedWrangler, 'workflow: selected Wrangler is not the approved minor');
	const root = document(source, 'workflow');
	let declared: string | undefined;
	walk(root, [], (path, value) => {
		const key = path.at(-1)!;
		const environment = key === 'env' &&
			(path.length === 1 || (path[0] === 'jobs' &&
				(path.length === 3 || (path.length === 5 && path[2] === 'steps'))));
		if (environment) mapping(value, 'workflow: unsupported dynamic environment');
		if (key.toUpperCase() !== 'WRANGLER_VERSION') return;
		requireValue(
			key === 'WRANGLER_VERSION' && path.length === 2 && path[0] === 'env' && declared === undefined,
			'workflow: duplicate or narrower WRANGLER_VERSION override'
		);
		declared = scalar(value, 'workflow.env.WRANGLER_VERSION');
	});
	function requirePin(pin: string) {
		requireValue(pin === expected, `workflow: executable Wrangler ${pin} must match approved ${expected}`);
	}
	if (declared !== undefined) requirePin(declared);
	function resolve(value: string) {
		if (/^\$\{\{\s*env\.WRANGLER_VERSION\s*\}\}$/.test(value)) {
			requireValue(declared !== undefined, 'workflow: missing WRANGLER_VERSION declaration');
			requirePin(declared);
		} else {
			requireValue(/^\d+\.\d+\.\d+$/.test(value), 'workflow: unsupported or dynamic Wrangler selection');
			requirePin(value);
		}
	}
	let count = 0;
	const jobs = child(root, 'jobs', 'workflow');
	for (const pair of jobs.items) {
		const job = mapping(pair.value, 'workflow job');
		const steps = job.get('steps', true);
		if (steps === undefined) continue;
		requireValue(isSeq(steps), 'workflow: unsupported steps mapping');
		for (const value of steps.items) {
			const step = mapping(value, 'workflow step');
			const action = step.get('uses', true);
			const uses = action === undefined ? undefined : scalar(action, 'step.uses');
			const inputs = step.get('with', true);
			requireValue(!uses?.startsWith('./'), 'workflow: unscanned local composite action is unsupported');
			requireValue(!uses || !/wrangler/i.test(uses) || /^cloudflare\/wrangler-action@/i.test(uses),
				'workflow: unsupported Wrangler action selection');
			if (uses !== undefined && /^cloudflare\/wrangler-action@/i.test(uses)) {
				const input = mapping(inputs, 'wrangler-action.with');
				const selections = input.items.filter((pair) =>
					scalar(pair.key, 'action input key').toLowerCase() === 'wranglerversion');
				requireValue(selections.length === 1, 'workflow: missing or ambiguous action Wrangler selection');
				// Action input whitespace is trimmed; mapping keys and env values are not.
				resolve(scalar(selections[0]!.value, 'wrangler-action.wranglerVersion').trim());
				count++;
			} else if (isMap(inputs)) {
				requireValue(!inputs.items.some((pair) =>
					scalar(pair.key, 'action input key').toLowerCase() === 'wranglerversion'),
					'workflow: unsupported action Wrangler selection');
			}
			const run = step.get('run', true);
			if (run !== undefined) count += cliSelections(scalar(run, 'step.run'), resolve);
		}
	}
	return count;
}
