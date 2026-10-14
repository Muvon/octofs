# pip-install install captures

Tool version: `pip 23.3.2 (Python 3.12.12)`

Captured on dev via `ssh dev`, no TTY, env `NO_COLOR=1 GIT_TERMINAL_PROMPT=0 DEBIAN_FRONTEND=noninteractive PAGER=cat GIT_PAGER=cat` (CI unset). Each scenario ran twice after the same reset step: once with separate streams (`.stdout`/`.stderr`), once with `2>&1` (`.merged`). Network timing, so the two runs can differ in timings/order.

Fresh `python3 -m venv` (bundled pip 23.3.2, Python 3.12.12) recreated before every scenario except already-satisfied. `PIP_CACHE_DIR` points to a scratch cache, which is wiped too. Requirement files are in src/. `venv-cap/bin/pip` is the absolute path under /tmp/octofs-digest-cap-installs/.

| scenario | command | exit (split/merged) | stdout B | stderr B | merged B | exercises |
|---|---|---|---|---|---|---|
| fresh-install | `/tmp/octofs-digest-cap-installs/venv-cap/bin/pip install -r requirements.txt` | 0/0 | 26687 | 124 | 26809 | 22 pinned requirements, about 70 resolved. `Collecting`/`Downloading` lines go to stdout, including final-state `━━━` progress bars (no CR; a single line each without a TTY). Then 'Installing collected packages' and one long 'Successfully installed' line. stderr holds only the `[notice] A new release of pip` banner. |
| already-satisfied | `/tmp/octofs-digest-cap-installs/venv-cap/bin/pip install -r requirements.txt` | 0/0 | 12958 | 124 | 13082 | Same requirements again: one 'Requirement already satisfied: X in ... (from ...)' line per package, on stdout. |
| resolve-conflict | `/tmp/octofs-digest-cap-installs/venv-cap/bin/pip install -r requirements-conflict.txt` | 1/1 | 2500 | 404 | 2905 | flask 3 with werkzeug 2.0.3: One error is SPLIT across streams. The two `ERROR:` lines (Cannot install ... / ResolutionImpossible) go to stderr, while the 'The conflict is caused by:' and 'To fix this you could try to:' block between them goes to stdout, after the 'INFO: pip is looking at multiple versions' line. |
| build-fail | `/tmp/octofs-digest-cap-installs/venv-cap/bin/pip install -r requirements-buildfail.txt` | 1/1 | 763 | 2244 | 3007 | psycopg2 from sdist, no pg_config. stdout ends with `Getting requirements to build wheel: finished with status 'error'`. stderr holds `error: subprocess-exited-with-error`, a box-drawn `× Getting requirements to build wheel did not run successfully.` block with the indented captured setup.py output (blank lines inside), and a 'note: This error originates from a subprocess' line. |
| no-matching-version | `/tmp/octofs-digest-cap-installs/venv-cap/bin/pip install -r requirements-noversion.txt` | 1/1 | 270 | 711 | 981 | flask==99.0.0: `ERROR: Could not find a version that satisfies the requirement` with the full list of versions, then `ERROR: No matching distribution found`. |
