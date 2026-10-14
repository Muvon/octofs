# phpunit captures

- Version: PHPUnit 12.5.39 (phar from phar.phpunit.de), PHP 8.4.21; dev's php emits startup warnings (base91 module API mismatch, yaml already loaded) on stderr in every run
- Env: NO_COLOR=1 GIT_TERMINAL_PROMPT=0 DEBIAN_FRONTEND=noninteractive PAGER=cat GIT_PAGER=cat, CI unset, no TTY (plain `ssh dev`), stdin=/dev/null
- Sources: src/gen.py generates tests/{pass,fail,error}; phpunit.phar in workdir (not kept)
- Files per scenario: `.stdout` + `.stderr` (one run), `.merged` (second run, `2>&1`)

| scenario | command | exit (sep/merged) | stdout | stderr | merged | exercises |
|---|---|---|---|---|---|---|
| pass-default | `php phpunit.phar tests/pass` | 0/0 | 475 | 262 | 737 | 240 passing tests in 6 files: progress dots + summary |
| pass-testdox | `php phpunit.phar --testdox tests/pass` | 0/0 | 3581 | 262 | 3843 | --testdox: per-test checkmark lines |
| fail-default | `php phpunit.phar tests/fail` | 2/2 | 2070 | 262 | 2332 | assertEquals array diff, assertSame multiline diff, RuntimeException, Error on null, E_USER_DEPRECATED, E_USER_WARNING, echo output (risky), skipped, incomplete, risky no-assertion test, data provider failures |
| fail-display-all | `php phpunit.phar --display-all-issues tests/fail` | 2/2 | 2571 | 262 | 2833 | same, --display-all-issues (deprecation/warning details) |
| fail-testdox | `php phpunit.phar --testdox tests/fail` | 2/2 | 4676 | 262 | 4938 | same, --testdox |
| syntax-error | `php phpunit.phar tests/error` | 255/255 | 1293 | 262 | 1555 | parse error in one test file among passing files |
