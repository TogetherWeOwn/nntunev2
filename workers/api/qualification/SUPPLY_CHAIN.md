# Qualification harness supply chain

This directory is its own pnpm root (`pnpm-workspace.yaml`, `pnpm-lock.yaml`).
It holds an isolated, secretless Miniflare harness importer. It does not share
a lockfile or `node_modules` with `apps/web`, and it needs no credentials.

## Selected dependencies (exact, no ranges)

| Package | Version | Role |
| --- | --- | --- |
| `miniflare` | `5.20261023.0-beta` | direct |
| `@esbuild/linux-x64` | `0.28.2` | direct (platform binary) |
| `workerd` | `1.20261023.1` | transitive, via `miniflare` |
| `@cloudflare/workerd-linux-64` | `1.20261023.1` | optional platform binary of `workerd` |

All integrities in `pnpm-lock.yaml` for packages that also appear in
`apps/web/pnpm-lock.yaml` are identical to that lock. Two packages exist only
here, because `apps/web` resolves other versions: `semver@7.8.6` and
`tslib@2.9.1`. Both were published more than three days before resolution and
declare no install hooks.

## Controls

- `pnpm-workspace.yaml`: `minimumReleaseAge: 4320` (minutes, pnpm 10.16+) and
  `ignoreScripts: true`. No exemptions, no `onlyBuiltDependencies`, no pnpmfile
  (none exists in the repo; `ignoreScripts` would not suppress one).
- `package.json`: private, `type: module`, no lifecycle scripts. The only script
  is `qualification` (`node --test runtime.test.mjs`).
- Lock generated with pnpm 10.34.9 using
  `pnpm install --lockfile-only --ignore-scripts`. Nothing was installed.

## Lifecycle-hook inventory (locked set, 99 package/snapshot entries)

Registry manifests were queried for every locked package for `preinstall`,
`install`, `postinstall` and `prepare`.

- Only `workerd@1.20261023.1` has an install hook: `postinstall: node install.js`.
- `undici` (`prepare`), `cookie` and `@sindresorhus/is` (`prepare`) have
  publish/dev-time scripts that do not run when installed as registry
  dependencies.
- `sharp@0.35.5` has no install hook; its platform binaries are optional
  `@img/*` packages with none either.
- `@esbuild/linux-x64` and `@cloudflare/workerd-linux-64` have no scripts.

### Review of the `workerd` postinstall

The tarball (integrity `sha512-NeofJFyaYNpqKpFRJW3ApuonaerPd6/JWlfTtLDYf2A6ygqrDEFnYgEXmc0hhOxqnJsSM+kXJN6VebPojzRrsg==`,
matching the lock) was inspected. `install.js` resolves the platform package
for the current OS/CPU, and if it is present only tries a hard-link
optimisation of `bin/workerd` and runs `workerd --version` to check the version.
If the optional package is missing it falls back to `npm install` or a direct
HTTPS download from the npm registry of the same-version platform package, with
no integrity check on that fallback path. With scripts ignored, none of this
runs. `bin/workerd` stays the Node shim, which resolves the integrity-locked
`@cloudflare/workerd-linux-64` binary directly. The script is therefore not
enabled and no exemption is granted. The unverified fallback download is
precisely the behaviour the ignored script would have added.

## Residual gaps

- Cargo has no native dependency age filter or blanket `build.rs`/proc-macro
  disable. The Rust crates in this repository remain covered only by lock
  review, `cargo deny` and isolated builds.
- `minimumReleaseAge` applies at resolution. It is not a retrospective audit of
  previously locked versions, and does not establish that the selected releases
  are safe. Packages in the lock that are not newly resolved here were compared
  by integrity with the existing web lock only.
- `miniflare` is a beta release; `workerd` and platform binaries are prebuilt
  native executables, trusted by registry integrity only.
- Not yet done: a clean `pnpm install --frozen-lockfile` and execution of
  `workerd` through `pnpm qualification`. These are pending the GitHub Actions
  run on the exact head and are not evidenced by any local run.
