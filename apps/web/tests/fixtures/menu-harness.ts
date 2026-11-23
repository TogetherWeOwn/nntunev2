import { mount, unmount } from 'svelte';
import ContextMenu from '../../src/lib/interactions/ContextMenu.svelte';
import { contextMenu, menuAt, unavailablePlaylistAction } from '../../src/lib/interactions/context-menu.js';

const row = document.createElement('div');
row.dataset.testid = 'fixture-row';
const invoker = document.createElement('button');
invoker.textContent = 'Open fixture menu';
const status = document.createElement('p');
status.setAttribute('role', 'status');
const play = document.createElement('button');
play.textContent = 'Play fixture';
play.setAttribute('data-context-menu-longpress', '');
play.onclick = () => { status.textContent = 'Playback activated'; };
row.append(play, invoker);
document.body.append(row, status);

function open(point?: { x: number; y: number }) {
	const instance = mount(ContextMenu, {
		target: document.body,
		props: {
			request: { ...menuAt(invoker, 'Fixture actions', [
				unavailablePlaylistAction,
				{ label: 'One choice', children: [{ label: 'Inline action', run: () => { status.textContent = 'Inline activated'; } }] },
				{ label: 'Destinations', children: [
					{ label: 'First destination', run: () => { status.textContent = 'First activated'; } },
					{ label: 'Second destination', run: () => { status.textContent = 'Second activated'; } }
				] }
			]), ...point },
			onclose: () => { void unmount(instance); }
		}
	});
}

contextMenu(row, (_invoker, point) => open(point));
invoker.onclick = () => open();
