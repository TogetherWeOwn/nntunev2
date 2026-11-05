# Self-hosted fonts

The app shell sets Figtree first with a system-ui fallback
(`--nt-font` in `tokens.css`). Files below ship from this directory so
the font-src self policy holds; nothing loads from a font CDN.

- Family: Figtree (sans-serif)
- Licence: SIL Open Font License 1.1; the full copyright and licence
  notice ships beside these files in [OFL.txt](./OFL.txt), from the
  [Google Fonts Figtree release](https://github.com/google/fonts/blob/main/ofl/figtree/OFL.txt).
- Copyright: 2022 The Figtree Project Authors
- Source: the Google Fonts release, via the Fontsource CDN
  (`figtree` package, latin subset, weights 400/700/800)
- Files: `figtree-latin-400-normal.woff2` (11,384 B),
  `figtree-latin-700-normal.woff2` (11,376 B),
  `figtree-latin-800-normal.woff2` (11,392 B)

Latin subset only: it covers the current product language. Wider
subsets land with the locales, through the same directory.
