"""Utilities for writing plugins."""

from inspect import signature, Parameter

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

    assert n_pos_only == 3, "target_def function should have three \
            positional-only arguments: loader, package, name"

    assert n_pos_only + n_kw_only == len(sig.parameters), \
            "target_def function must only have positional-only and keyword-only \
            parameters"

    rewrites = {}
    for p in sig.parameters:
        parm = sig.parameters[p]
        if parm.kind == Parameter.KEYWORD_ONLY:
            if parm.annotation is Rewrite:
                rewrites[p] = Rewrite
            elif parm.annotation is Delta:
                rewrites[p] = Delta

    def wrapper(loader, package, name, **kw):
        for arg in kw:
            if rewrite[arg] is not None:
                kw[arg] = rewrite[arg](kw[arg])
        return fn(loader, package, name, **kw)

    return wrapper 

class Rewrite:
    pass

def _rewrite(s):
    return s

class Delta:
    pass

def _delta(s):
    return s


