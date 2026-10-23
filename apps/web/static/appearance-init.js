/* Appearance pre-paint init (render-blocking, same-origin).
 *
 * Applies a saved Light/Dark override before the browser paints the
 * server-rendered page, so the first revealed frame already shows the
 * chosen palette instead of flashing the OS one. Loaded with a plain
 * `src` script tag from app.html: no inline code, so the existing
 * strict CSP (`script-src 'self'`, no `unsafe-inline`) is unchanged.
 *
 * Reads only the validated appearance enum from device-local storage;
 * never writes, never contacts a backend. Unknown, missing or
 * unreadable values leave the OS palette in place until the root
 * layout mounts and resolves System explicitly.
 */
(function () {
	'use strict';
	try {
		var raw = window.localStorage.getItem('nntune:appearance:v1');
		if (raw === 'light' || raw === 'dark') {
			document.documentElement.setAttribute('data-appearance', raw);
		}
	} catch {
		/* Storage refused: the OS palette stands; the mounted layout
		 * keeps working with an in-memory choice. */
	}
})();
