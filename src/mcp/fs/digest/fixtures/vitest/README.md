# vitest captures

- Version: vitest 5.0.2 + vite 8, node v22.20.0 (nvm; vitest 5 refuses node 18), project-local npm install
- Env: NO_COLOR=1 GIT_TERMINAL_PROMPT=0 DEBIAN_FRONTEND=noninteractive PAGER=cat GIT_PAGER=cat, CI unset, no TTY (plain `ssh dev`), stdin=/dev/null
- Sources: src/genjs.py <dir> vitest generates tests/{pass,fail,syntax}; workdir = project root
- Files per scenario: `.stdout` + `.stderr` (one run), `.merged` (second run, `2>&1`)

| scenario | command | exit (sep/merged) | stdout | stderr | merged | exercises |
|---|---|---|---|---|---|---|
| pass-default | `npx vitest run tests/pass` | 0/0 | 951 | 0 | 1204 | 240 passing tests in 6 files, console.log in 2 files ('stdout | file > suite > test' blocks) |
| pass-verbose | `npx vitest run --reporter=verbose tests/pass` | 0/0 | 14606 | 0 | 14610 | --reporter=verbose: one line per test |
| fail-default | `npx vitest run tests/fail` | 1/1 | 1218 | 4935 | 6169 | toEqual object/array diffs, multiline string diff with blank lines, thrown Error with stack, TypeError, console.log/warn/error then fail, async rejection with blank lines, 300ms timeout, it.each failures, skip, todo |
| fail-verbose | `npx vitest run --reporter=verbose tests/fail` | 1/1 | 11328 | 4935 | 16250 | same, --reporter=verbose |
| syntax-import-error | `npx vitest run tests/syntax` | 1/1 | 420 | 1633 | 2039 | file with syntax error + file importing a missing module -> 'Failed Suites' blocks |
| no-tests-found | `npx vitest run tests/nomatch` | 1/1 | 54 | 148 | 202 | filter matching no files -> 'No test files found', exit 1 |
