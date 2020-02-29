"""Utilities for writing plugins.

A Cobble plugin should import '*' from this module for ease of implementation.
"""

import sys
from inspect import signature, Parameter
import cobble.env

def target_def(fn):
    """Takes a function definition with a certain shape, wraps it in validation
    code, and registers it as a package-verb."""

    # Validate function shape:
    # - 3 positional-only arguments
    # - some number of keyword-only arguments

    sig = signature(fn)
    n_pos_only = sum(1 for p in sig.parameters if sig.parameters[p].kind ==
            Parameter.POSITIONAL_ONLY)
    n_kw_only = sum(1 for p in sig.parameters if sig.parameters[p].kind ==
            Parameter.KEYWORD_ONLY)

    assert n_pos_only == 2, "target_def function should have 2 \
            positional-only arguments: package, name"

    assert n_pos_only + n_kw_only == len(sig.parameters), \
            "target_def function must only have positional-only and keyword-only \
            parameters"

    rewrites = {}
    for p in sig.parameters:
        parm = sig.parameters[p]
        if parm.kind == Parameter.KEYWORD_ONLY:
            if parm.annotation is Delta:
                rewrites[p] = cobble.env.prepare_delta

    def wrapper(package, name, **kw):
        for arg in kw:
            r = rewrites.get(arg)
            if r is not None:
                kw[arg] = r(kw[arg])
        return fn(package, name, **kw)

    mod = sys.modules[fn.__module__]
    if not hasattr(mod, 'package_verbs'):
        mod.package_verbs = {}
    assert fn.__name__ not in mod.package_verbs, \
            "Module %s contains multiple verbs called %s" % (fn.__module__, fn.__name__)
    mod.package_verbs[fn.__name__] = wrapper

    return wrapper 

class Delta:
    """Marker type for attributing parameters that should be prepared as deltas
    by @target_def."""
    pass

# Provide a targeted subset of this module to plugins using `import *`.
__all__ = ["target_def", "Delta"]
