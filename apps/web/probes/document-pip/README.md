# Document Picture-in-Picture real-API probe

Throwaway harness that answers one question with the real browser API
(no fakes): can headless Chromium open a Document PiP window, mount a
Svelte component into `pipWindow.document.body`, clone the opener's
same-origin styles into the PiP document, and observe `pagehide` on
close? This is not product UI and nothing here is shipped with the app.

Run it from `apps/web` after `pnpm install` (and
`pnpm exec playwright install chromium` once):

```sh
pnpm exec vite build probes/document-pip --config probes/document-pip/vite.config.ts --outDir dist
node probes/document-pip/run.mjs
```

Set `PROBE_CHANNEL=chromium` to drive the full Chromium channel instead
of the default headless shell.

## Recorded result (2026-10-26): GO

Playwright's default headless shell, Chrome for Testing 156.0.8078.4:

- `documentPictureInPicture` present in a secure (localhost) context;
  `requestWindow({ width: 400, height: 240 })` resolved on a real
  button click (transient activation).
- Svelte `mount()` rendered the probe component into the PiP document
  (`mountedText: "Probe mounted"`).
- Cloned same-origin stylesheet rules applied in the PiP document
  (computed `color: rgb(255, 0, 0)` on the mounted node).
- `pipWindow.close()` from the opener fired `pagehide` on the PiP
  window.

So the mini-player build can test against the real API in headless
Chromium instead of only structural fakes. The controller's fake-based
unit tests live in `src/lib/player/documentPip.test.ts`.
