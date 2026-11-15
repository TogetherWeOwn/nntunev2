# Design layer: tokens, icons, strings

Single source of truth for the app-shell visual system. Three modules,
one budget, no competitors: token names are all `--nt-*`, the typeface
is openly licensed, and every icon stencil below is drawn for this
project.

## Tokens (`tokens.css`)

The nine color tokens (`--nt-bg`, `--nt-bg-raised`, `--nt-fg`,
`--nt-fg-muted`, `--nt-accent`, `--nt-accent-fg`, `--nt-border`,
`--nt-good`, `--nt-bad`) keep their names and values in every palette.
Change one only by updating `tokens.test.ts` first, in the open, with
the reason recorded — the pins fail otherwise.

The second layer adds, never renames:

- **Surface scale** (`--nt-surface-pane/hover/press/tint`, `--nt-scrim`,
  `--nt-shadow-overlay`, `--nt-hero-scrim`). Raised surfaces alias
  `--nt-bg-raised` so cards track the palette.
- **Type scale** (`--nt-text-2xs` … `--nt-text-display`, `--nt-leading-*`,
  `--nt-weight-body/strong/display`). Pairings: `2xs`/`xs`/`sm` with
  `--nt-leading-sm`; `base`/`lg`/`xl`/`2xl` with `--nt-leading-base`;
  `3xl`/`4xl`/display with their matching
  `--nt-leading-3xl`/`4xl`/display. Only 400/700/800 ship, deliberately
  — no mid-weight, so hosts must not synthesize one. Body copy stays on
  the existing `--nt-line` until migrated; new text uses
  `--nt-leading-base`. Display clamps below the measured shell maximum.
- **Space** (`--nt-space-px-2` … `--nt-space-px-64`, pixel-true; the
  `space-1..5` steps stay canonical: 4/8/16/24/40 px at default size).
- **Radii, sizes, z-order, motion** (`--nt-radius-*`, `--nt-control-*`,
  `--nt-rail-w`/`--nt-panel-w` families, `--nt-z-*`, `--nt-dur-*`,
  `--nt-ease-*`). Entries stay slower than exits; motion tokens pair
  with the reduced-motion rule in `app.css`. `--nt-control-sm` (32px)
  sits below the `--nt-target-coarse` (40px) floor: it is fine-pointer
  only, and touch targets still keep the coarse minimum.

### Contrast contract

Text pairs hold 4.5:1, graphics and the focus ring 3:1 — all computed
from the shipped file in `tokens.test.ts`, not copied from a table.

- Semantic status text uses `--nt-good-text` / `--nt-bad-text`, never
  the plain `--nt-good` / `--nt-bad` tokens alone. The `-text` tokens
  equal good/bad in dark and darken in light to hold 4.5:1 on both the
  page background and the pane with headroom.
- Plain good/bad are graphic and icon colour (3:1 floor).
- The focus ring is `--nt-focus-ring` (the accent) with
  `--nt-focus-ring-width` (2px) and `--nt-focus-ring-offset` (2px); it
  holds 3:1 everywhere controls sit — page, pane, raised and hover
  surfaces. Never reach for `--nt-border` as a ring: it fails 3:1 in
  the light palette.

### Motion rule

Non-essential animation scopes under
`@media (prefers-reduced-motion: no-preference)` in addition to the
global reduce rule. Durations and times render with tabular figures.

## Icons (`icons.ts`, `Icon.svelte`)

49 stencils for the R1/R2 surfaces on a 24-unit grid in `currentColor`.
Stroked chrome at weight 2 with round caps; solid fills for transport
glyphs, status dots and the liked heart. Hosts render through `Icon`:

- Decorative: `<Icon name="search" />` renders `aria-hidden`.
- Actionable: pass the visible action name —
  `<Icon name="play" label="Play" />`. The name describes the action or
  state, never the file name. Labelled (`role="img"`) mode is for
  standalone icons only: inside an already-named button or link the icon
  stays decorative, otherwise screen readers announce the action twice.

The player imports only `TRANSPORT_ICON_PATHS` and uses the same SVG
renderer extracted into `IconSvg.svelte`. This keeps unused stencils out
of the active bundle without deleting any named icon. The internal
renderer accepts trusted kit stencil constants only, never user content;
its sizing and accessibility attributes are identical to `Icon`.

Script budget: all four icon files compile to at most 8 KiB of
uncompressed, minified client JavaScript (`icons.test.ts` retains the
original gate; `icons-renderer.builder.test.ts` includes the extracted
renderer with the pinned Svelte/TypeScript compilers and Terser). Each module is
measured independently with all exports retained, including component
runtime imports, but not the shared Svelte runtime already in the app.
No gzip or route tree-shaking discount is applied. Deferred stencils for
later surfaces — radio, devices, share, link, picture-in-picture,
captions — land with their screens by adding one name, one stencil and
one label row each.

## Strings (`strings.ts`)

`design.icon.*` keys with English defaults. Keys ship now so
localisation is a translation file later. Actionable hosts still pass
the visible action name as the label; the table keeps named uses and
checks honest in the meantime.
