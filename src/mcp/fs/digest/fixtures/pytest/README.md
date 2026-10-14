# pytest captures

- Version: pytest 9.1.1, Python 3.12.12, plugins anyio-4.12.1 asyncio-1.4.0 (preinstalled on dev)
- Env: NO_COLOR=1 GIT_TERMINAL_PROMPT=0 DEBIAN_FRONTEND=noninteractive PAGER=cat GIT_PAGER=cat, CI unset, no TTY (plain `ssh dev`), stdin=/dev/null
- Sources: src/gen.py generates py/{pass,fail,collect}; workdir = that dir
- Files per scenario: `.stdout` + `.stderr` (one run), `.merged` (second run, `2>&1`)

| scenario | command | exit (sep/merged) | stdout | stderr | merged | exercises |
|---|---|---|---|---|---|---|
| pass-default | `python3 -m pytest` | 0/0 | 2165 | 0 | 2165 | 270 passing tests in 6 files, some print (captured, hidden) and 4 DeprecationWarnings -> warnings summary |
| pass-verbose | `python3 -m pytest -v` | 0/0 | 23344 | 0 | 23344 | same, -v: one PASSED line per test |
| fail-default | `python3 -m pytest` | 1/1 | 10962 | 0 | 10962 | 222 tests: assert diffs (dict/list/multiline str with blank lines), chained exception traceback, KeyError in generator, captured stdout/stderr, pytest.fail with blank lines, setup+teardown ERRORs, parametrize failures, skip/skipif/xfail/xpass/strict-xpass, warnings, 1.2s sleep |
| fail-verbose | `python3 -m pytest -v` | 1/1 | 28566 | 0 | 28566 | same, -v |
| fail-rA-durations | `python3 -m pytest -rA --durations=5` | 1/1 | 24173 | 0 | 24173 | same, -rA (PASSED/SKIPPED/XFAIL lines in short summary + passes captured output section) and --durations=5 |
| collect-error-default | `python3 -m pytest` | 2/2 | 3531 | 0 | 3531 | SyntaxError + ModuleNotFoundError + ImportError at collection -> 'Interrupted: 3 errors during collection', exit 2 |
| collect-error-verbose | `python3 -m pytest -v` | 2/2 | 3590 | 0 | 3590 | same, -v |
| collect-error-continue | `python3 -m pytest --continue-on-collection-errors` | 1/1 | 3691 | 0 | 3691 | --continue-on-collection-errors: 120 tests run + 3 collection errors, exit 1 |
| quiet-fail | `python3 -m pytest -q` | 1/1 | 10389 | 0 | 10389 | fail project with -q (dots only, no header) |
| no-tests-found | `python3 -m pytest -k nomatch_xyz` | 5/5 | 519 | 0 | 519 | -k matching nothing -> all deselected, exit 5 |
