# go-test captures

- Version: go version go1.26.4 (Red Hat 1.26.4-1.el10_2) linux/amd64, GOTOOLCHAIN=local
- Env: NO_COLOR=1 GIT_TERMINAL_PROMPT=0 DEBIAN_FRONTEND=noninteractive PAGER=cat GIT_PAGER=cat, CI unset, no TTY (plain `ssh dev`), stdin=/dev/null
- Sources: src/gen.py generates go/{pass,fail,build,panic,timeout} modules; src/run.sh is the exact driver
- Files per scenario: `.stdout` + `.stderr` (one run), `.merged` (second run, `2>&1`)

| scenario | command | exit (sep/merged) | stdout | stderr | merged | exercises |
|---|---|---|---|---|---|---|
| pass-default | `go test -count=1 ./...` | 0/0 | 257 | 0 | 257 | 6 packages + 1 no-test package, all pass (-count=1): ok lines + '[no test files]' |
| pass-verbose | `go test -count=1 -v ./...` | 0/0 | 15206 | 0 | 15206 | same, -v: === RUN / --- PASS / --- SKIP, t.Logf lines, subtests '#00' |
| pass-cached | `go test ./... >/dev/null 2>&1; go test ./...` | 0/0 | 269 | 0 | 269 | warm run without -count=1: '(cached)' suffixes |
| fail-default | `go test -count=1 ./...` | 1/1 | 1506 | 0 | 1506 | 4 passing pkgs + orders (t.Errorf/t.Fatalf, t.Log, multi-line messages with blank lines, subtests, fmt.Println stdout, skip) + inventory failures + no-test pkg |
| fail-verbose | `go test -count=1 -v ./...` | 1/1 | 12396 | 0 | 12396 | same, -v |
| fail-single-pkg | `go test -count=1 ./orders` | 1/1 | 1014 | 0 | 1014 | go test ./orders only (no 'ok' lines from other packages) |
| build-fail-default | `go test -count=1 ./...` | 1/1 | 178 | 556 | 734 | compile errors in 2 files of one package, test-only undefined symbol, vet printf failure, one good pkg; [build failed]/[setup failed] lines; compiler output on stderr |
| build-fail-verbose | `go test -count=1 -v ./...` | 1/1 | 1923 | 556 | 2479 | same, -v |
| panic-default | `go test -count=1 ./...` | 1/1 | 2032 | 0 | 2027 | nil deref panic in test, index-out-of-range panic in goroutine, panic(error) with blank lines; goroutine dumps |
| panic-verbose | `go test -count=1 -v ./...` | 1/1 | 3974 | 0 | 3970 | same, -v |
| timeout-default | `go test -count=1 -timeout 3s ./...` | 1/1 | 1356 | 0 | 1356 | -timeout 3s with a 10s sleeping test -> 'panic: test timed out', goroutine dump |
| race-pass | `go test -count=1 -race ./auth ./billing` | 0/0 | 71 | 0 | 71 | -race on 2 packages (needs cgo); see exit code |
