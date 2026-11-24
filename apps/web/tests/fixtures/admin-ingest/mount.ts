import { mount } from 'svelte';
import PrivateIngest from '../../../src/lib/admin-ingest/PrivateIngest.svelte';
import type { IngestRequests } from '../../../src/lib/admin-ingest/requests.js';
import '../../../src/app.css';

declare global {
	interface Window {
		syntheticAccount: 'owner' | 'member' | 'anonymous';
		enableIngestFixture?: boolean;
		uploadFixture(bytes: number[]): ReturnType<IngestRequests['upload']>;
		queueFixture(query: Parameters<IngestRequests['queue']>[0]): ReturnType<IngestRequests['queue']>;
		detailFixture(jobId: string): ReturnType<IngestRequests['detail']>;
		retryFixture(jobId: string, body: { revision: number }): ReturnType<IngestRequests['retry']>;
		detailAbortedAtReply?: boolean;
	}
}

mount(PrivateIngest, {
	target: document.getElementById('app')!,
	props: {
		enabled: window.enableIngestFixture,
		role: window.syntheticAccount,
		requests: {
			async upload(file) {
				return window.uploadFixture(Array.from(new Uint8Array(await file.arrayBuffer())));
			},
			queue: (query) => window.queueFixture(query),
			async detail(id, signal) {
				const reply = await window.detailFixture(id);
				window.detailAbortedAtReply = signal.aborted;
				return reply;
			},
			retry: (id, body) => window.retryFixture(id, body)
		}
	}
});
