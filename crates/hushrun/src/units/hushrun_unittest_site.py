# hushrun's unittest adapter (ADR 0110, spec C-66). hushrun writes this file as `sitecustomize.py` in
# a directory of its own, and puts that directory first on PYTHONPATH for a check whose COMMAND runs
# the standard library's unittest (`python -m unittest`, Django's `manage.py test`). Python imports
# it as it starts (site.py), before any of COMMAND's code. It is written so that any Python parses
# it: it loads the adapter on Python 3.10 or later, in a process whose own command line runs the
# tests, and then hands over to the sitecustomize it hides, if there is one, as Python would have
# loaded it. On any error of its own the adapter does nothing, and the tests run as without it.
import os
import sys


def _hushrun_adapter():
    if sys.version_info < (3, 10) or not os.environ.get("HUSHRUN_UNITS"):
        return
    try:
        import hushrun_unittest_impl

        hushrun_unittest_impl.start()
    except Exception:  # never stop the tests: a run with nothing written is one hushrun didn't see
        pass


def _hushrun_hidden():
    """The sitecustomize this one hides: the next on sys.path, run as `sitecustomize`."""
    here = os.path.dirname(os.path.abspath(__file__))
    try:
        import importlib.machinery
        import importlib.util

        rest = [p for p in sys.path if os.path.abspath(p or os.curdir) != here]
        spec = importlib.machinery.PathFinder.find_spec("sitecustomize", rest)
    except Exception:
        return
    if spec is None or spec.loader is None:
        return
    try:
        module = importlib.util.module_from_spec(spec)
        sys.modules["sitecustomize"] = module
        spec.loader.exec_module(module)
    except Exception as err:
        # As site.py says of its own sitecustomize's error, and goes on.
        sys.stderr.write(
            "Error in sitecustomize; set PYTHONVERBOSE for traceback:\n%s: %s\n"
            % (type(err).__name__, err)
        )


# The hidden one first: it may set up what COMMAND's Python needs (a finder, a path) before any of
# the repository's code loads, and hushrun's finder then goes in front of whatever it added.
_hushrun_hidden()
_hushrun_adapter()
