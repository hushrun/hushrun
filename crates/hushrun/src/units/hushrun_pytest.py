# hushrun's pytest adapter (ADR 0068, spec C-32). hushrun writes this file and loads it with
# PYTEST_ADDOPTS=-p hushrun_pytest and PYTHONPATH when a check's `tests` isn't off. It is written
# so that any Python parses it: the adapter itself needs Python 3.8 or later (PEP 578 audit hooks),
# and on anything older, or after any error of its own, it does nothing, and pytest runs as without
# it.
import sys

if sys.version_info >= (3, 8):
    try:
        from hushrun_pytest_impl import *  # noqa: F401,F403
    except Exception:  # never stop pytest: a run with nothing written is a run hushrun didn't see
        pass
