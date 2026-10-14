# go-install install captures

Tool version: `go1.26.4 (Red Hat 1.26.4-1.el10_2) linux/amd64`

Captured on dev via `ssh dev`, no TTY, env `NO_COLOR=1 GIT_TERMINAL_PROMPT=0 DEBIAN_FRONTEND=noninteractive PAGER=cat GIT_PAGER=cat` (CI unset). Each scenario ran twice after the same reset step: once with separate streams (`.stdout`/`.stderr`), once with `2>&1` (`.merged`). Network timing, so the two runs can differ in timings/order.

Scratch module (src/main.go imports gin, cobra, logrus, zap, gorm and its sqlite driver (cgo), go-redis, prometheus, uuid, x/sync). GOMODCACHE and GOCACHE are scratch dirs, `GOFLAGS=-modcacherw`, `GOTOOLCHAIN=local`, default `GOPROXY=https://proxy.golang.org,direct`. Reset = wipe both caches and install the named go.mod/go.sum (the .tidy pair comes from the first scenario). go writes everything to STDERR.

| scenario | command | exit (split/merged) | stdout B | stderr B | merged B | exercises |
|---|---|---|---|---|---|---|
| mod-tidy-fresh | `go mod tidy` | 0/0 | 0 | 4564 | 4564 | go.mod with no requires: `go: finding module for package X`, `go: downloading M vV` and `go: found X in M vV` lines. |
| mod-download-fresh | `go mod download -x` | 0/0 | 0 | 29239 | 29239 | `go mod download -x`: 354 `# get <proxy URL>` lines, each later paired with `# get <URL>: 200 OK (0.004s)`. There are no `go: downloading` lines in -x mode. This is the large, noisy fetch log. |
| build-fresh | `go build -o /dev/null .` | 0/0 | 0 | 2024 | 2024 | `go build` with a complete go.mod/go.sum and an empty cache: `go: downloading` lines only, no compile output. |
| build-cached | `go build -o /dev/null .` | 0/0 | 0 | 0 | 0 | Same build with warm caches: no output at all (0 bytes), exit 0. |
| build-badversion | `go build -o /dev/null .` | 1/1 | 0 | 128 | 128 | logrus v9.9.9 (invalid major version): `go: errors parsing go.mod:` and a `go.mod:8:2:` error. Nothing is fetched. |
| mod-tidy-badversion | `go mod tidy` | 1/1 | 0 | 128 | 128 | Same go.mod under `go mod tidy`: identical parse error. |
| mod-download-unknownrev | `go mod download` | 1/1 | 0 | 581 | 581 | Valid-looking but unresolvable require (logrus v1.99.0 + a nonexistent github module): proxy 404 falls back to a direct git ls-remote. `fatal: could not read Username ... terminal prompts disabled` is wrapped in a go error, with tab-indented continuation lines. |
| build-unknownrev | `go build -o /dev/null .` | 1/1 | 0 | 1564 | 1564 | Same go.mod under `go build`: a 'missing go.sum entry' error per import, each with a tab-indented `go get`/`go mod download` hint line. |
| mod-tidy-unknownrev | `go mod tidy` | 1/1 | 0 | 17221 | 17221 | Same go.mod under `go mod tidy`: go: downloading lines, then one multi-line import-chain error per affected package (`go: X imports` / tab-indented chain / error / 'Confirm the import path' / 'If this is a private repository'). Many near-duplicate blocks. |
