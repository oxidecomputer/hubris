import hashlib
import pickle
import string
import types
from inspect import signature
from functools import reduce

class EnvKey:
    """Represents a key that can be used in environments."""

    def __init__(self, name, *,
            from_literal = None,
            combine = None,
            default = None,
            readout = None):
        """Creates a key with a name and strategy functions.

        The strategy functions are:
        - from_literal: used to translate a literal written in a BUILD file to
          the appropriate internal value type. If `None`, any literal is
          accepted.
        - combine: used to merge different values for the same key. If `None`,
          different values are an error condition. If the combine function
          returns `None`, the key is deleted.
        - default: value to be read when an environment doesn't have a value
          for the key.
        - readout: used to prepare a value for processing. Can be omitted if no
          preparation (beyond from_literal/combine) is needed.
        """

        self.name = name
        self._from_literal = from_literal
        self._combine = combine
        self._default = freeze(default)
        self._readout = readout

    def from_literal(self, literal):
        """Produces a value legal for this key from `literal`, or throws."""
        if self._from_literal is not None:
            return self._from_literal(literal)
        else:
            return literal

    def combine(self, lhs, rhs):
        """Combines two values for this key, or throws if they can't be combined."""
        assert self._combine is not None, \
                "Environment key %s requires a unique value, got two: %r and %r" \
                % (self.name, lhs, rhs)
        return self._combine(lhs, rhs)

    def readout(self, value):
        if self._readout is not None:
            return self._readout(value)
        else:
            return value

    @property
    def default(self):
        return self._default

def overrideable_string_key(name):
    """Makes an EnvKey with a given 'name' that will accept a single string and
    allow overrides."""
    def from_literal(lit):
        assert isinstance(lit, str)
        return lit
    return EnvKey(
        name,
        from_literal = from_literal,
        combine = lambda lhs, rhs: rhs,
    )

def overrideable_bool_key(name):
    """Makes an EnvKey with a given 'name' that will accept a single bool and
    allow overrides."""
    def from_literal(lit):
        assert isinstance(lit, bool)
        return lit
    return EnvKey(
        name,
        from_literal = from_literal,
        combine = lambda lhs, rhs: rhs,
    )

def appending_string_seq_key(name, readout = None):
    """Makes an EnvKey with a given 'name' that will accept sequences of
    strings and combine them by appending to yield a tuple."""
    def from_literal(lit):
        # A string can be iterated as a list of strings. This would produce
        # confusing behavior. Avoid this by checking for str first.
        assert not isinstance(lit, str) \
                and all(isinstance(e, str) for e in lit), \
                "Expected list of strings, got: %r" % lit
        return tuple(lit)
    return EnvKey(
        name,
        from_literal = from_literal,
        combine = lambda lhs, rhs: lhs + rhs,
        default = (),
        readout = readout,
    )

def prepending_string_seq_key(name):
    """Makes an EnvKey with a given 'name' that will accept sequences of
    strings and combine them by prepending. (This is niche, but relevant when
    linking C programs.)"""
    def from_literal(lit):
        # A string can be iterated as a list of strings. This would produce
        # confusing behavior. Avoid this by checking for str first.
        assert not isinstance(lit, str) \
                and all(isinstance(e, str) for e in lit), \
                "Expected list of strings, got: %r" % lit
        return tuple(lit)
    return EnvKey(
        name,
        from_literal = from_literal,
        combine = lambda lhs, rhs: rhs + lhs,
        default = (),
    )

def frozenset_key(name, readout = None):
    """Makes an EnvKey with a given 'name' that will accept iterables of
    strings and combine them into a unique frozen set."""
    def from_literal(lit):
        # A string can be iterated as a list of strings. This would produce
        # confusing behavior. Avoid this by checking for str first.
        assert not isinstance(lit, str) \
                and all(isinstance(e, str) for e in lit), \
                "Expected list of strings, got: %r" % lit
        return frozenset(lit)

    return EnvKey(
        name,
        from_literal = from_literal,
        combine = lambda lhs, rhs: lhs | rhs,
        default = frozenset(),
        readout = readout,
    )


class KeyRegistry(object):
    """Keeps track of environment key definitions."""

    def __init__(self):
        self._keys = {}

    def define(self, key):
        """Defines a new environment key.

        key: must be an EnvKey with a unique name.
        """
        assert type(key) is EnvKey, \
                "Expected EnvKey, got: %r" % key
        assert self._keys.get(key.name) is None, \
                "Key %s defined twice: first %r, then %r" % (
                        key.name, self._keys[key.name], key)
        self._keys[key.name] = key

    def __contains__(self, key):
        return self._keys.__contains__(key)

    def __iter__(self):
        return self._keys.__iter__()

    def __getitem__(self, name):
        return self._keys.__getitem__(name)

    def get(self, name):
        return self._keys.get(name)

    def __len__(self):
        return len(self._keys)

class Env(object):
    def __init__(self, registry, prototype_dict = {}, *, _fresh = False):
        self._registry = registry
        if _fresh:
            self._dict = prototype_dict
        else:
            self._dict = {}
            for k, v in prototype_dict.items():
                self._dict[k] = freeze(v)
        self._memoized_digest = None

    def __eq__(self, other):
        # TODO: should we include the dict here, or rely on the digest?
        return self.registry is other.registry \
                and self.digest == other.digest

    def __hash__(self):
        return hash(self.digest)

    # Iterable/dict interface

    def __contains__(self, key):
        return self._dict.__contains__(key)

    def __iter__(self):
        return self._dict.__iter__()

    def __getitem__(self, key):
        key_def = self._registry.get(key)
        if key_def is not None:
            return key_def.readout(self._dict.get(key, key_def.default))
        else:
            raise AttributeError("Use of undefined environment key %r" % key)

    def __len__(self):
        return self._dict.__len__()

    def subset(self, keys):
        """Creates a new Env by deleting any keys not present in the given list."""
        return Env(
            self._registry,
            dict((k, v) for k, v in self._dict.items() if k in keys),
            _fresh = True,
        )

    def copy_defaults(self, keys):
        d = dict(self._dict)
        for k in keys:
            if k not in d:
                d[k] = self._registry[k].default
        return Env(self._registry, d, _fresh = True)

    def subset_require(self, keys):
        """Returns an environment that contains the same values as 'self' for
        the keys named in 'keys', and no others. This operation is used for
        combination environment-filtering and error-checking.

        If any key in 'keys' is missing in 'self', but the associated key
        definition specifies a default value, the default value is copied into
        the result.

        If no default value is available, it's an error.
        """
        e = self.subset(keys).copy_defaults(keys)
        e.require(keys)
        return e

    def without(self, matcher):
        if isinstance(matcher, types.FunctionType):
            d = dict((k, v) for k, v in self._dict.items() if matcher(k))
        elif isinstance(matcher, (tuple, list, set, frozenset)):
            d = dict((k, v) for k, v in self._dict.items() if k not in matcher)
        else:
            raise TypeError("Unexpected matcher: %r" % matcher)

        return Env(self._registry, d, _fresh = True)

    def readout_all(self):
        return dict((k, self[k]) for k in self._dict)

    def derive(self, delta):
        """Creates a new Env that is identical to this one except for the
        changes made by 'delta'.

        Several types of primitives are accepted as deltas:

        - Functions/lambdas. Functions are expected to take an environment as
          their only argument, and return an environment.

        - Dicts. Dict keys are environment key names; dict values are literal
          expressions that will be converted to the appropriate type for the
          environment key.
        """
        if type(delta) is types.FunctionType \
                and len(signature(delta).parameters) == 1:
            return delta(self)
        elif type(delta) is dict:
            # Make a shallow copy of our backing dict.
            new_dict = dict(self._dict)
            # Apply each key in the delta to the copy.
            for k, v in delta.items():
                key_def = self._registry[k]
                if key_def is None:
                    raise Exception("delta contained unknown key %s (=%r)"
                        % (k, v))
                v = self.rewrite(v)
                v = key_def.from_literal(v)
                if k in self._dict:
                    new_value = key_def.combine(self[k], v)
                    if new_value is None:
                        del new_dict[k]
                    else:
                        new_dict[k] = new_value
                else:
                    new_dict[k] = v

            # aaaand we're done. Inform the constructor that the dict is fresh
            # to avoid an extra copy.
            return Env(self._registry, new_dict, _fresh = True)
        elif isinstance(delta, (list, tuple)):
            return reduce(lambda env, delt: env.derive(delt), delta, self)
        elif delta is None:
            return self
        else:
            raise Exception("delta should be func or dict, got: %r" % delta)

    @property
    def digest(self):
        if self._memoized_digest is None:
            # To make contents predictable, make a sorted list of key-value
            # tuples. Normalize the values while we're here.
            contents = sorted((k, _normalize(v)) for k, v in self._dict.items())
            # To make contents binary, pickle them. Fix the protocol revision
            # so we get more consistent results.
            binary = pickle.dumps(contents, protocol = 3)
            # To make the length of the contents predictable, hash them.
            self._memoized_digest = hashlib.sha1(binary).hexdigest()
        return self._memoized_digest

    def rewrite(self, literal):
        if isinstance(literal, str):
            # The actual processing code, yaaaay
            return string.Template(literal).substitute(self)
        elif isinstance(literal, tuple):
            return tuple(self.rewrite(elt) for elt in literal)
        elif isinstance(literal, frozenset):
            return frozenset(self.rewrite(elt) for elt in literal)
        else:
            return literal

    def require(self, keys):
        """Asserts that this environment contains values for every key named in
        'keys'. Default values count if the key's default value is not None."""
        missing = [k for k in keys \
                if k not in self._dict and self._registry[k].default is None]
        assert not missing, \
                "Required keys %r missing from environment" % missing

def _normalize(x):
    """Takes a frozen datum and converts sets to sorted tuples.

    The result is still a frozen datum, technically, but set semantics are
    erased. The result is mostly useful for generating predictable environment
    hashes.

    It's probably not necessary for this function to be particularly efficient,
    because it's contributing to env digests, which are already expensive and
    so memoized.
    """
    if isinstance(x, (str, bool)) or x is None:
        return x
    if isinstance(x, frozenset):
        return tuple(sorted(_normalize(e) for e in x))
    assert isinstance(x, tuple)
    return x


def freeze(x):
    """Attempts to make x immutable.

    Input can be str, bool, set, frozenset, list, tuple, None, and any nesting
    of those.

    Output will consist of str, bool, frozenset, tuple, and None only.
    """
    if isinstance(x, str):
        # Assume that strings are immutable.
        return x
    elif isinstance(x, bool):
        # Bools too
        return x
    elif isinstance(x, (set, frozenset)):
        return frozenset(freeze(v) for v in x)
    elif isinstance(x, (list, tuple)):
        return tuple(freeze(v) for v in x)
    elif x is None:
        return None
    else:
        raise TypeError("Value cannot be frozen for use in an environment: %r" %
                x)

def is_frozen(x):
    """Checks whether 'x' could result from a call to 'freeze'."""
    return isinstance(x, (str, bool)) \
            or x is None \
            or (isinstance(x, (frozenset, tuple)) and all(is_frozen(e) for e in
                x))

def prepare_delta(d):
    if isinstance(d, types.FunctionType):
        return d
    elif isinstance(d, dict):
        return dict((k, freeze(v)) for k, v in d.items())
    elif d is None:
        return None
    elif isinstance(d, (list, tuple)):
        return tuple(prepare_delta(e) for e in d)
    else:
        raise TypeError("invalid delta: %r" % d)

def is_delta(d):
    if d is None:
        return True
    if isinstance(d, types.FunctionType):
        return True
    if isinstance(d, dict):
        return all(isinstance(k, str) and is_frozen(v) for k,v in d.items())
    if isinstance(d, (list, tuple)):
        return all(is_delta(e) for e in d)
    return False

def _deps_from_literal(deps):
    # Check types
    assert isinstance(deps, (list, tuple)), "deps must be list, got: %r" % deps
    assert all(isinstance(e, str) for e in deps), \
            "deps elements must be strings, got: %r" % deps
    # Freeze, then unique and refreeze
    return frozenset(freeze(deps))

DEPS_KEY = EnvKey(
    name = '__deps__',
    from_literal = _deps_from_literal,
    # combine is frozenset union
    combine = lambda prev, new: prev | new,
    default = frozenset(),
)
