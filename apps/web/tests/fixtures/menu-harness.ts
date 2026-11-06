import { mount, unmount } from 'svelte';
import ContextMenu from '../../src/lib/interactions/ContextMenu.svelte';
import { menuAt, unavailablePlaylistAction } from '../../src/lib/interactions/context-menu.js';

const invoker = document.createElement('button');
invoker.textContent = 'Open fixture menu';
document.body.append(invoker);
const status = document.createElement('p');
status.setAttribute('role', 'status');
document.body.append(status);
invoker.onclick = () => {
	const instance = mount(ContextMenu, {
		target: document.body,
		props: {
			request: menuAt(invoker, 'Fixture actions', [
				unavailablePlaylistAction,
				{ label: 'One choice', children: [{ label: 'Inline action', run: () => { status.textContent = 'Inline activated'; } }] },
				{ label: 'Destinations', children: [
					{ label: 'First destination', run: () => { status.textContent = 'First activated'; } },
					{ label: 'Second destination', run: () => { status.textContent = 'Second activated'; } }
				] }
			]),
			onclose: () => { void unmount(instance); }
		}
	});
};
