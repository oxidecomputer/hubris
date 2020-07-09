"""Environments and supporting types.

An *environment* in Cobble is an immutable key-value mapping, similar to a
Python dict. The contents of environments are restricted, however, in two ways:

1. Each environment is associated with a `KeyRegistry`, and any key used in the
   environment must be registered before use.

2. The values that can be stored in the environment are different for each key,
   but are generally limited to simple Python data: lists, tuples, sets,
   strings, and booleans.

Cobble targets may produce different build steps in different environments --
for example, a C binary can be built in several different environments, each of
which gives a slightly different value for the `c_flags` key.
"""

import hashlib
import pickle
import string
import types
from inspect import signature
from functools import reduce

class EnvKey:
    """Represents a key that can be used in environments.

    This is the definition of a key that gets stored with the `KeyRegistry`.
    """

    def __init__(self, name, *,
            from_literal = None,
            combine = None,
            default = None,
            readout = None,
            help = None):
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

        'help' optionally gives a message explaining what the key is for, which
        will be printed if it is required but not found.
        """

        self.name = name
        self._from_literal = from_literal
        self._combine = combine
        self._default = freeze(default)
        self._readout = readout
        self.help = help

    def from_literal(self, literal):
        """Produces a value legal for this key from `literal`, or throws."""
        if self._from_literal is not None:
            return self._from_literal(literal)
        else:
            return literal

    def combine(self, lhs, rhs):
        """Combines two values for this key, or throws if they can't be
        combined."""
        assert self._combine is not None, (
                "Environment key %s requires a unique value, got two: %r and %r"
                % (self.name, lhs, rhs))
        return self._combine(lhs, rhs)

    def readout(self, value):
        """Processes `value` for inclusion in a build file. Most keys don't
        need custom readout."""
        if self._readout is not None:
            return self._readout(value)
        else:
            return value

    @property
    def default(self):
        """Gets the default value for this key, or `None`."""
        return self._default

def overrideable_string_key(name, default = None, readout = None,
        help = None):
    """Makes an EnvKey with a given 'name' that will accept a single string and
    allow overrides."""
    def from_literal(lit):
        assert isinstance(lit, str)
        return lit
    return EnvKey(
        name,
        from_literal = from_literal,
        combine = lambda lhs, rhs: rhs,
        default = default,
        readout = readout,
        help = help,
    )

def overrideable_bool_key(name, readout = None, default = None, help = None):
    """Makes an EnvKey with a given 'name' that will accept a single bool and
    allow overrides."""
    def from_literal(lit):
        assert isinstance(lit, bool)
        return lit
    return EnvKey(
        name,
        from_literal = from_literal,
        combine = lambda lhs, rhs: rhs,
        readout = readout,
        default = default,
        help = help,
    )

def appending_string_seq_key(name, readout = None, default = (), help = None):
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
        default = tuple(freeze(e) for e in default),  # defensive copy
        readout = readout,
        help = help,
    )

def prepending_string_seq_key(name, default = (), readout = None, help = None):
    """Makes an EnvKey with a given 'name' that will accept sequences of
    strings and combine them by prepending. When extended by a delta at several
    points in the build graph, this will order items produced by most-derived
    targets first. (This is niche, but relevant when linking C programs.)"""
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
        default = tuple(freeze(e) for e in default),
        readout = readout,
        help = help,
    )

def frozenset_key(name, readout = None, default = (), help = None):
    """Makes an EnvKey with a given 'name' that will accept iterables of
    strings and combine them into a unique frozen set."""
    def from_literal(lit):
        # A string can be iterated as a list of strings. This would produce
        # confusing behavior. Avoid this by checking for str first.
        assert not isinstance(lit, str) \
                and all(isinstance(e, str) for e in lit), \
                "Expected collection of strings for key %s, got: %r" % (name, lit)
        return frozenset(lit)

    return EnvKey(
        name,
        from_literal = from_literal,
        combine = lambda lhs, rhs: lhs | rhs,
        default = frozenset(freeze(e) for e in default),
        readout = readout,
        help = help,
    )


class KeyRegistry(object):
    """Keeps track of environment key definitions."""

    def __init__(self):
        self._keys = {}

    def define(self, key):
        """Defines a new environment key.

        key: must be an EnvKey with a name that is unique in this registry.
        """
        assert type(key) is EnvKey, \
                "Expected EnvKey, got: %r" % key
        assert self._keys.get(key.name) is None, \
                "Key %s defined twice: first %r, then %r" % (
                        key.name, self._keys[key.name], key)
        self._keys[key.name] = key

    # Mapping-like implementation

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
    """An immutable mapping from keys to values, which can be extended in
    predictable ways.

    Environments can compute a *digest* of their contents, which is a
    hexadecimal string. Cobble doesn't promise to derive the digest using any
    particular means, and the means may change in later versions.

    registry: the key registry for this environment.
    prototype_dict: an initial key-value mapping for this environment. Keys and
    values must be legal for the registry."""
    def __init__(self, registry, prototype_dict = {}, *, _fresh = False):
        self._registry = registry
        if _fresh:
            # Undocumented parameter _fresh is used to indicate that the dict
            # does not need to be defensively copied/frozen. This is used
            # within Cobble to construct environments with less memory traffic.
            self._dict = prototype_dict
        else:
            # Assume that the dict can contain arbitrary mutable nonsense, and
            # that the caller maintained a reference to it.
            self._dict = {}
            for k, v in prototype_dict.items():
                self._dict[k] = freeze(v)
        # Digest will be computed on-demand.
        self._memoized_digest = None

    # Equality / hash

    def __eq__(self, other):
        # We include the registry to treat environments from different
        # registries as disjoint.
        # We include the digest as a quick way of establishing inequality.  We
        # compare the entire dict to avoid the potential for digest collisions,
        # which is vanishingly small, and would cause the build system to
        # become incorrect, but hey -- let's be obviously correct here.
        return self._registry is other._registry \
                and self.digest == other.digest \
                and self._dict == other._dict

    def __hash__(self):
        # Fast, constant-time hashing for environments whose digest has already
        # been computed. Forces computation of the digest for other
        # environments.
        return hash(self.digest)

    # Iterable/dict interface

    def __contains__(self, key):
        return self._dict.__contains__(key)

    def __iter__(self):
        return self._dict.__iter__()

    def __getitem__(self, key):
        """The `__getitem__` implementation applies the key's readout function,
        which may format or otherwise prepare the result, and falls back to the
        key's default if present.
        """
        # This implementation has the side effect that, if a key not present in
        # the registry somehow makes its way into self._dict, getitem will not
        # admit its presence.
        key_def = self._registry.get(key)
        if key_def is not None:
            return key_def.readout(self._dict.get(key, key_def.default))
        else:
            raise KeyError("Use of undefined environment key %r" % key)

    def __len__(self):
        return self._dict.__len__()

    def subset(self, keys):
        """Creates a new Env by deleting any keys not present in the given
        list/set."""
        return Env(
            self._registry,
            dict((k, v) for k, v in self._dict.items() if k in keys),
            _fresh = True,
        )

    def subset_require(self, keys):
        """Returns an environment that contains the same values as 'self' for
        the keys named in 'keys', and no others. This operation is used for
        combination environment-filtering and error-checking.

        If any key in 'keys' is missing in 'self', but the associated key
        definition specifies a default value, the default value is copied into
        the result.

        The keys are interpreted as being required for success: if no default
        value is available, it's an error.
        """
        assert all(isinstance(k, str) for k in keys)
        e = self.subset(keys)._copy_defaults(keys)
        e.require(keys)
        return e

    def _copy_defaults(self, keys):
        """Produces a new Env containing the contents of this one, plus the
        defaults for any missing keys."""
        d = dict(self._dict)
        for k in keys:
            if k not in d:
                default = self._registry[k].default
                if default is not None:
                    d[k] = self._registry[k].default
        return Env(self._registry, d, _fresh = True)

    def without(self, matcher):
        """Returns a new Env that contains the same mappings as this one,
        except for keys specified by 'matcher'.

        'matcher' can be a predicate function taking one argument (a key name);
        if it returns true, the key will be removed from the result.

        'matcher' can also be a collection, in which case any key that is 'in
        matcher' will be removed from the result.
        """
        if isinstance(matcher, types.FunctionType):
            d = dict((k, v) for k, v in self._dict.items() if matcher(k))
        elif isinstance(matcher, (tuple, list, set, frozenset)):
            d = dict((k, v) for k, v in self._dict.items() if k not in matcher)
        else:
            raise TypeError("Unexpected matcher: %r" % matcher)

        return Env(self._registry, d, _fresh = True)

    def readout_all(self):
        """Returns a 'dict' representation of this environment containing all
        keys with explicit values. The values are passed through the readout
        function for each key, equivalent to `self[k]`.

        This can be used to prepare a version of this environment for use with
        the `ninja_syntax` module, or for easy debug printing, etc.
        """
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
                key_def = self._registry.get(k)
                if key_def is None:
                    raise Exception("delta contained unknown key %s (=%r)"
                        % (k, v))
                v = self.rewrite(v)
                v = key_def.from_literal(v)
                if k in self._dict:
                    new_value = key_def.combine(self._dict[k], v)
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
        """Reads out the environment digest for 'self'.

        The environment digest is a 'str' containing a hexadecimal number. It
        is computed such that two environments with different contents will
        have different digests. This means that comparing the digests of two
        environments is an inexpensive way of telling if they are identical.
        (Ignoring, for the moment, a very small risk of collisions in the
        digest function.)

        The digest is computed on demand the first time it is requested, and
        then stored, so that later requests are cheap. This avoids unnecessary
        work for short-lived intermediate environments.

        The method for computing the digest is unspecified, i.e. Cobble may
        change it in the future and you shouldn't rely on it.
        """
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
        """Rewrites 'literal' using information from this environment,
        returning the rewritten version.

        This implements the user-visible templating language available in BUILD
        files.

        Rewrites proceed as follows:

        - For `str`, any environment key named with "$key" or "${key}" is
          replaced by the value in this environment, as processed by the key's
          readout function. If the key is missing, it's an error.

        - For tuples or frozensets, each element is rewritten recursively.

        - Booleans and None are returned verbatim.
        """
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
        if missing:
            msg = "Required keys %r missing from environment" % missing
            for m in missing:
                h = self._registry[m].help
                if h is None:
                    msg += '\n- \'%s\' has no description' % m
                else:
                    msg += '\n- \'%s\': %s' % (m, h)

            raise AssertionError(msg)

def freeze(x):
    """Attempts to make x immutable by converting it into a *frozen datum*.

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
    """Checks whether 'x' is a frozen datum, something that could result from a
    call to 'freeze'."""
    return isinstance(x, (str, bool)) \
            or x is None \
            or (isinstance(x, (frozenset, tuple)) and all(is_frozen(e) for e in
                x))

def _normalize(x):
    """Takes a frozen datum and converts sets to sorted tuples, ensuring that
    any two data with the same logical contents have the same printed contents.

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

def prepare_delta(d):
    """Creates an environment delta from one of a few possible data types.

    'd' may be:

    - a function that takes an environment as a parameter and returns a new
      environment.

    - a 'dict', whose keys are environment key names as 'str', and whose values
      can be passed to 'freeze'. When applied to an environment, the resulting
      delta will pass the dict value and the old value (if any) to each key's
      `combine` function.

    - 'None', which means no changes.

    - A list or tuple, which specifies a sequence of deltas to apply in order;
      each element will be passed to 'prepare_delta' recursively.
    """
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
    """Checks if 'd' is a plausible environment delta."""
    if d is None:
        return True
    if isinstance(d, types.FunctionType):
        return True
    if isinstance(d, dict):
        return all(isinstance(k, str) and is_frozen(v) for k,v in d.items())
    if isinstance(d, (list, tuple)):
        return all(is_delta(e) for e in d)
    return False
