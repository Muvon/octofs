# docker-install install captures

Tool version: `Docker 29.4.3, buildx v0.33.0`

Captured on dev via `ssh dev`, no TTY, env `NO_COLOR=1 GIT_TERMINAL_PROMPT=0 DEBIAN_FRONTEND=noninteractive PAGER=cat GIT_PAGER=cat` (CI unset). Each scenario ran twice after the same reset step: once with separate streams (`.stdout`/`.stderr`), once with `2>&1` (`.merged`). Network timing, so the two runs can differ in timings/order.

docker build with BuildKit (buildx v0.33.0, default docker driver). No TTY, so `--progress=auto` picks the plain `#N ...` format. Base image python:3.12-slim was pulled beforehand. Each build runs apt-get install and pip install inside the image (src/Dockerfile, src/Dockerfile.fail, src/requirements.txt, src/app.py). docker writes everything to STDERR. The tool output inside RUN steps contains CR characters (apt and pip progress), which are passed through raw: 155 CRs in build-fresh.

| scenario | command | exit (split/merged) | stdout B | stderr B | merged B | exercises |
|---|---|---|---|---|---|---|
| build-fresh | `docker build --no-cache -t octofs-digest-cap:ok .` | 0/0 | 0 | 25153 | 25148 | `--no-cache`: `#N [i/7] RUN ...` step headers, timestamped `#N 1.234 <line>` apt and pip output, `#N DONE 3.2s`, exporting/naming steps. |
| build-cached | `docker build -t octofs-digest-cap:ok .` | 0/0 | 0 | 1062 | 1067 | Every RUN/COPY step is `#N CACHED`. Steps appear out of Dockerfile order ([5/7] before [4/7]) because BuildKit numbers them by solve order. |
| build-fail | `docker build --no-cache -f Dockerfile.fail -t octofs-digest-cap:fail .` | 1/1 | 0 | 24076 | 24080 | `RUN pip install psycopg2` fails: the pip build error inside `#N` lines, `#N ERROR: process ... exit code: 1`, a `------` block that repeats the last log lines, the Dockerfile excerpt with a `>>>` marker, and a final `ERROR: failed to build: failed to solve: ...` line. |
