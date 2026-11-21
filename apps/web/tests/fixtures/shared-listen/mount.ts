import { notifyLocalAuthTransition } from '../../../src/lib/auth-transition.js';
import { LIBRARY_AUTH_TRANSITION_KEY } from '../../../src/lib/library/cache.js';
import type { ListenEvent } from '../../../src/lib/player/observeListenEvents.js';
import { createPlayerListenHistory } from '../../../src/lib/player/playerListenHistory.js';

// Like the signed-in shell, the audio owner lives outside the route outlet.
const outlet = document.querySelector('main')!;
function renderRoute() {
	const heading = document.createElement('h1');
	heading.textContent = location.pathname === '/library' ? 'Fixture Library' : 'Fixture Home';
	outlet.replaceChildren(heading);
}
for (const link of document.querySelectorAll('nav a')) link.addEventListener('click', (event) => {
	event.preventDefault();
	history.pushState(null, '', link.getAttribute('href')!);
	renderRoute();
});
window.addEventListener('popstate', renderRoute);
renderRoute();

const audio = document.querySelector('audio')!;
const read = (): ListenEvent[] => JSON.parse(sessionStorage.getItem('fixture-listens') ?? '[]');
const record = (event: ListenEvent) => sessionStorage.setItem('fixture-listens', JSON.stringify([...read(), event]));
let grant: string | undefined;
let pending: ListenEvent | undefined;
let held = false;
const observer = createPlayerListenHistory({
	send(event) { if (held) pending = event; else record(event); },
	async close() {},
	dispose() { pending = undefined; }
});
const detach = observer.attach(audio, 'fixture-track', () => grant);

const fixture = {
	read,
	issue: (id: string) => { grant = id; },
	media: (name: string) => audio.dispatchEvent(new Event(name)),
	setEnded: (ended: boolean) => Object.defineProperty(audio, 'ended', { configurable: true, value: ended }),
	abandon: () => observer.finish(audio),
	detach,
	hold: () => { held = true; },
	pending: () => pending?.event,
	release: () => { held = false; if (pending) record(pending); pending = undefined; },
	reset: (source: 'local' | 'storage') => {
		if (source === 'local') notifyLocalAuthTransition();
		else dispatchEvent(new StorageEvent('storage', { key: LIBRARY_AUTH_TRANSITION_KEY }));
	},
	transition: (type: 'pagehide' | 'pageshow') => dispatchEvent(new PageTransitionEvent(type, { persisted: true }))
};
declare global { interface Window { sharedListenFixture: typeof fixture } }
window.sharedListenFixture = fixture;
