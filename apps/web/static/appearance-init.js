// Read the saved override before first paint. Blocked storage leaves the CSS default.
(() => {
	try {
		const appearance = localStorage.getItem('nntune:appearance:v1');
		if (appearance === 'light' || appearance === 'dark') {
			document.documentElement.setAttribute('data-appearance', appearance);
		}
	} catch { /* Appearance storage is optional. */ }
})();
