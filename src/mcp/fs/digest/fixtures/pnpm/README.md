# pnpm-install install captures

Tool version: `pnpm 8.15.9 via npx (node v18.12.1)`

Captured on dev via `ssh dev`, no TTY, env `NO_COLOR=1 GIT_TERMINAL_PROMPT=0 DEBIAN_FRONTEND=noninteractive PAGER=cat GIT_PAGER=cat` (CI unset). Each scenario ran twice after the same reset step: once with separate streams (`.stdout`/`.stderr`), once with `2>&1` (`.merged`). Network timing, so the two runs can differ in timings/order.

pnpm run through `npx -y pnpm@8.15.9` (pre-fetched, so npx download noise is not in the captures). The store is a scratch dir (`npm_config_store_dir`). Same package.json as npm-install. pnpm writes everything to STDOUT, including WARN and ERR lines; stderr is empty in every scenario.

| scenario | command | exit (split/merged) | stdout B | stderr B | merged B | exercises |
|---|---|---|---|---|---|---|
| fresh-install | `npx -y pnpm@8.15.9 install` | 0/0 | 5088 | 0 | 4123 | `Progress: resolved N, reused N, downloaded N, added N` snapshot lines, then ` WARN  deprecated` lines and a box-drawn 'Update available!' banner. The banner appears only in the split-stream run because the update check is cached afterwards. Then `Packages: +705`, a `+++` bar, per-package lifecycle-script output prefixed `.../sqlite3@5.1.6/node_modules/sqlite3 install:`, and a dependencies/devDependencies list with '(x is available)' notes. |
| cached-reinstall | `npx -y pnpm@8.15.9 install` | 0/0 | 773 | 0 | 773 | Lockfile present, store warm: 'Lockfile is up to date', progress, 'Done in'. |
| fail-e404 | `npx -y pnpm@8.15.9 add this-package-does-not-exist-octofs-xyz@1.0.0` | 1/1 | 421 | 0 | 509 | ` ERR_PNPM_FETCH_404 ` error header with GET URL and explanation. |
| fail-badversion | `npx -y pnpm@8.15.9 add lodash@99.0.0` | 1/1 | 454 | 0 | 366 | ` ERR_PNPM_NO_MATCHING_VERSION ` error with the latest-version hint. |
