# uv-install install captures

Tool version: `uv 0.13.1 (Python 3.12.12)`

Captured on dev via `ssh dev`, no TTY, env `NO_COLOR=1 GIT_TERMINAL_PROMPT=0 DEBIAN_FRONTEND=noninteractive PAGER=cat GIT_PAGER=cat` (CI unset). Each scenario ran twice after the same reset step: once with separate streams (`.stdout`/`.stderr`), once with `2>&1` (`.merged`). Network timing, so the two runs can differ in timings/order.

uv was installed with pip into its own venv (venv-uv). It installs into a separate target venv (`uv venv`, then `VIRTUAL_ENV` set). The target venv and `UV_CACHE_DIR` are recreated before every scenario except already-satisfied. Same requirement files as pip-install. `U` = /tmp/octofs-digest-cap-installs/venv-uv/bin/uv. uv writes everything to STDERR; stdout is empty.

| scenario | command | exit (split/merged) | stdout B | stderr B | merged B | exercises |
|---|---|---|---|---|---|---|
| fresh-install | `/tmp/octofs-digest-cap-installs/venv-uv/bin/uv pip install -r requirements.txt` | 0/0 | 0 | 2183 | 2184 | 'Using Python ... environment at:', 'Resolved 71 packages', `Downloading <pkg> (size)` / ` Downloaded <pkg>` pairs for large wheels, 'Prepared', 'Installed 71 packages', then one ` + pkg==ver` line per package. |
| already-satisfied | `/tmp/octofs-digest-cap-installs/venv-uv/bin/uv pip install -r requirements.txt` | 0/0 | 0 | 110 | 110 | 'Checked 22 packages in 2ms' only. |
| resolve-conflict | `/tmp/octofs-digest-cap-installs/venv-uv/bin/uv pip install -r requirements-conflict.txt` | 1/1 | 0 | 381 | 381 | `error: No solution found when resolving dependencies` followed by `cause:` lines; continuation lines are indented 9 spaces. With NO_COLOR there is no `×`/`╰─▶` box drawing. |
| build-fail | `/tmp/octofs-digest-cap-installs/venv-uv/bin/uv pip install -r requirements-buildfail.txt` | 1/1 | 0 | 2158 | 2158 | `error: Failed to build psycopg2==2.9.10`, `cause:` chain, captured backend [stdout]/[stderr] blocks indented 9 spaces (blank lines inside), and a `hint:` line. |
| no-matching-version | `/tmp/octofs-digest-cap-installs/venv-uv/bin/uv pip install -r requirements-noversion.txt` | 1/1 | 0 | 279 | 279 | `error: No solution found ...` plus one `cause:` line naming flask==99.0.0. uv stops at the first unsatisfiable requirement, so the nonexistent package is not mentioned. |
