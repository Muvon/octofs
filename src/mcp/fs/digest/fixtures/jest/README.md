# jest captures

- Version: jest 30.5.2, node v22.20.0 (nvm; jest 30 crashes on node 18.12), project-local npm install
- Env: NO_COLOR=1 GIT_TERMINAL_PROMPT=0 DEBIAN_FRONTEND=noninteractive PAGER=cat GIT_PAGER=cat, CI unset, no TTY (plain `ssh dev`), stdin=/dev/null
- Sources: src/genjs.py <dir> jest generates tests/{pass,fail,syntax}; workdir = project root
- Files per scenario: `.stdout` + `.stderr` (one run), `.merged` (second run, `2>&1`)

| scenario | command | exit (sep/merged) | stdout | stderr | merged | exercises |
|---|---|---|---|---|---|---|
| pass-default | `npx jest tests/pass` | 0/0 | 0 | 951 | 936 | 240 passing tests in 6 files with console.log (jest prints console blocks + PASS lines to stderr) |
| pass-verbose | `npx jest --verbose tests/pass` | 0/0 | 548 | 5742 | 6320 | --verbose: per-test checkmarks |
| fail-default | `npx jest tests/fail` | 1/1 | 0 | 6789 | 6804 | same failure mix as vitest (diffs, thrown errors, console.*, async rejection, 300ms timeout, it.each, skip, todo) |
| fail-verbose | `npx jest --verbose tests/fail` | 1/1 | 1118 | 9482 | 10537 | same, --verbose |
| syntax-import-error | `npx jest tests/syntax` | 1/1 | 0 | 1154 | 1169 | babel SyntaxError + 'Cannot find module' -> 'Test suite failed to run' |
| no-tests-found | `npx jest tests/nomatch` | 1/1 | 360 | 0 | 360 | path matching nothing -> 'No tests found, exiting with code 1' |
