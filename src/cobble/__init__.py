import types


class Target:
    """Represents a user-level target in the build graph.

    This is the type that target constructors exposed from plugins are expected
    to return.
    """

    def __init__(self,
            concrete = False,
            down = None,
            using = None,
            local = None):
        """Creates a new Target.

        concrete: when set, this target can be built independently
        down, using, local: functions that alter the respective environments; each
                            can be None to make no changes.
        """
        assert type(concrete) is bool, "concrete: expected bool, got %r" % concrete
        assert _is_env_func(down), "down: expected unary func, got %r" % down
        assert _is_env_func(using), "using: expected unary func, got %r" % using
        assert _is_env_func(local), "local: expected unary func, got %r" % local

        self.concrete = concrete
        self.down = down
        self.using = using
        self.local = local

def _is_env_func(f):
    """Checks if f can be supplied where a function over environments is
    expected."""

    if f is None:
        return True

    return (type(f) is types.FunctionType) and len(signature(f).params) == 1

