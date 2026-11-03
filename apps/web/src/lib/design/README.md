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
  `--nt-weight-body/strong/display`). Body copy stays on the existing
  `--nt-line`; display clamps below the measured shell maximum.
- **Space** (`--nt-space-px-2` … `--nt-space-px-64`, pixel-true; the
  `space-1..5` steps stay canonical: 4/8/16/24/40 px at default size).
- **Radii, sizes, z-order, motion** (`--nt-radius-*`, `--nt-control-*`,
  `--nt-rail-w`/`--nt-panel-w` families, `--nt-z-*`, `--nt-dur-*`,
  `--nt-ease-*`). Entries stay slower than exits; motion tokens pair
  with the reduced-motion rule in `app.css`.

### Contrast contract

Text pairs hold 4.5:1, the focus ring 3:1 — both computed from the
shipped file in `tokens.test.ts`, not copied from a table.

One fence: **light `--nt-good` on the pane is 4.42**, so semantic text
there uses `--nt-fg` plus a good/bad icon, never the good token alone.
Darkening the value instead is a test-author-flow change, not a silent
tweak.

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
  state, never the file name.

Script budget: the three icon files hold 8 KB of content at most
(`icons.test.ts` enforces it; production minification and tree-shaking
only shrink further). Deferred stencils for later surfaces — radio,
devices, share, link, picture-in-picture, captions — land with their
screens by adding one name, one stencil and one label row each.

## Strings (`strings.ts`)

`design.icon.*` keys with English defaults. Keys ship now so
localisation is a translation file later. Actionable hosts still pass
the visible action name as the label; the table keeps named uses and
checks honest in the meantime.
