# rspec captures

- Version: RSpec 3.13 (rspec-core 3.13.6) installed via gem --install-dir into scratch, ruby 3.3.10; dev's ruby prints 'Ignoring ffi-1.15.5 ...' on stderr
- Env: NO_COLOR=1 GIT_TERMINAL_PROMPT=0 DEBIAN_FRONTEND=noninteractive PAGER=cat GIT_PAGER=cat, CI unset, no TTY (plain `ssh dev`), stdin=/dev/null
- Sources: src/gen.py generates spec/{pass,fail,error}; GEM_HOME=rb/gems
- Files per scenario: `.stdout` + `.stderr` (one run), `.merged` (second run, `2>&1`)

| scenario | command | exit (sep/merged) | stdout | stderr | merged | exercises |
|---|---|---|---|---|---|---|
| pass-default | `/tmp/octofs-digest-cap-tests/rb/gems/bin/rspec spec/pass` | 0/0 | 333 | 0 | 333 | 240 passing examples: progress dots + summary |
| pass-documentation | `/tmp/octofs-digest-cap-tests/rb/gems/bin/rspec --format documentation spec/pass` | 0/0 | 3438 | 0 | 3436 | --format documentation: one line per example |
| fail-default | `/tmp/octofs-digest-cap-tests/rb/gems/bin/rspec spec/fail` | 1/1 | 4017 | 71 | 4088 | hash diff, multiline string diff with blank lines, ArgumentError backtrace, NoMethodError on nil, puts/warn then fail, aggregate_failures, skip, xit, pending (fails as expected / fixed), deprecation warning |
| fail-documentation | `/tmp/octofs-digest-cap-tests/rb/gems/bin/rspec --format documentation spec/fail` | 1/1 | 6496 | 71 | 6567 | same, --format documentation |
| load-syntax-error | `/tmp/octofs-digest-cap-tests/rb/gems/bin/rspec spec/error` | 1/1 | 2452 | 0 | 2448 | SyntaxError + LoadError while loading spec files -> 'errors occurred outside of examples' |
