"""High-level build targets."""

import types
import cobble.env
from cobble.env import is_delta
from itertools import chain
from functools import reduce
import os.path

# Key used to accumulate implicit dependency edges for Ninja.
IMPLICIT = cobble.env.frozenset_key('__implicit__')

# Key used to accumulate order-only (i.e. not time-sensitive) dependency edges
# for Ninja.
ORDER_ONLY = cobble.env.frozenset_key('__order_only__')

KEYS = frozenset([IMPLICIT, ORDER_ONLY])

class Target(object):
    """A high-level, parameterizable, build target."""

    def __init__(self, package, name, *,
            using_and_products,
            down = None,
            local = None,
            deps = [],
            concrete = False,
            ):
        """Creates a target.

        package: the Package object representing the package where this target
            was declared.
        name: name of this target
        down, local: environment deltas for computing the down- and local-env,
            respectively.
        using_and_products: function to compute the using-delta and products
            list in a given env.
        deps: list of target identifiers that this target depends upon. The
              identifiers in this list *may* contain references to environment
              keys.
        """
        assert isinstance(name, str)
        assert is_delta(down)
        assert is_delta(local)
        assert isinstance(using_and_products, types.FunctionType)

        # Process relative deps so plugins don't have to
        deps = tuple(package.make_absolute(d) for d in deps)

        self.package = package
        self._name = name
        self._concrete = concrete

        self._down = down
        self._using_and_products = using_and_products
        self._local = local
        self._deps = deps

        self._evaluate_memos = {}
        self._transparent = False

    @property
    def ident(self):
        return '//' + self.package.relpath + ':' + self._name

    def stats(self):
        return {
            'unique_environments': len(self._evaluate_memos),
        }

    def derive_down(self, env):
        """Derives the down-environment, that is, the environment seen by
        dependencies of this target."""
        return env.derive(self._down)

    def derive_local(self, env):
        """Derives the local-environment provided to using_and_products."""
        return env.derive(self._local)

    def using_and_products(self, local_env):
        """Computes the list of products produced by this target in the given
        environment, as well as the using-delta that affects any target
        depending on this one. (These are computed together because they often
        involve the same computation.)"""

        using, products = self._using_and_products(self, local_env)
        # Internal check for misbehaving implementations
        assert is_delta(using), "impl returned bad delta: %r" % using
        assert isinstance(products, (list, tuple, set, frozenset)), \
                "impl returned bad products: %r" % products
        return (using, products)

    @property
    def name(self):
        return self._name

    def evaluate(self, env_up):
        if env_up not in self._evaluate_memos:
            self._evaluate_memos[env_up] = self._evaluate(env_up)
        return self._evaluate_memos[env_up]

    def _evaluate(self, env_up):
        deps_key = cobble.env.DEPS_KEY.name

        env_down = self.derive_down(env_up)
        # Derive a first version of our local environment, which won't contain
        # any using-deltas applied by our deps. We need this to evaluate our
        # deps!
        env_local_0 = self.derive_local(env_down)
        # Rewrite key references in our deps list, producing a concrete deps list.
        deps = env_local_0.rewrite(self._deps)
        # Resolve all the identifiers and evaluate the targets.
        deps = (self.package.find_target(id) for id in deps)
        evaluated_deps = [dep.evaluate(env_down) for dep in deps]
        # The evaluated_deps list has the shape:
        #  [ ( {(target, env): (rank, using)}, {(target, env): [product]} ) ]

        # Merge the first maps together.
        merged = _topo_merge(m[0] for m in evaluated_deps)
        # Merge the second maps together.
        # TODO: should a target-env pair appear with *different* products
        # lists, this implementation will hide it!
        products = dict(chain(*(m[1].items() for m in evaluated_deps)))

        # Extract all the using-deltas in the order they should be applied.
        dep_usings = (u for (t, e), (r, u) in _topo_sort(merged))
        env_local_1 = reduce(lambda e, dlt: e.derive(dlt), dep_usings, env_local_0)

        self._check_local(env_local_1)

        our_using, our_products = self._using_and_products(env_local_1)

        if not self._transparent:
            # discard info about *our* dependencies rather than communicate it
            # to our user.
            merged.clear()

        merged[(self, env_up)] = (0, our_using)
        products[(self, env_up)] = our_products

        return (merged, products)

    def _check_local(self, env):
        # TODO implement checks
        pass


def _topo_sort(mapping):
    """Orders target graph info dicts topologically."""
    def key(pair):
        (t, e), (r, u) = pair
        return (r, t.ident, e.digest if e else None, u)
    return sorted(mapping.items(), key = key)

def _topo_merge(dicts):
    """Takes a list of target graph info dicts and merges them into a new,
    higher-rank dict."""
    merged = {}

    for (target, env), (rank, using) in chain(*(m.items() for m in dicts)):
        rank += 1
        if (target, env) in merged:
            # This target appears in the graph more than once! Take the higher rank.
            rank = max(rank, merged[(target, env)][0])
        merged[(target, env)] = (rank, using)

    return merged

_special_product_keys = frozenset([IMPLICIT.name, ORDER_ONLY.name])

class Product(object):
    def __init__(self,
            env,
            outputs,
            rule,
            inputs = None,
            implicit = None,
            order_only = None,
            symlink_as = None):
        self.env = env
        self.inputs = inputs
        self.rule = rule
        self.outputs = outputs
        self.symlink_as = symlink_as

        self.implicit = env[IMPLICIT.name]
        if implicit: self.implicit |= frozenset(implicit)
        self.order_only = env[ORDER_ONLY.name]
        if order_only: self.order_only |= frozenset(order_only)

        self.variables = env.without(_special_product_keys).readout_all()

    def ninja_dicts(self):
        d = {
            'outputs': self.outputs,
            'rule': self.rule,
        }
        if self.inputs: d['inputs'] = self.inputs
        if self.implicit: d['implicit'] = self.implicit
        if self.order_only: d['order_only'] = self.order_only
        if self.variables: d['variables'] = self.variables

        if self.symlink_as:
            assert len(self.outputs) == 1, \
                    "target wanted symlink but has too many outputs"
            s = {
                'outputs': [self.symlink_as],
                'rule': 'cobble_symlink_product',
                'order_only': self.outputs,
                'variables': {
                    'target': os.path.relpath(self.outputs[0],
                        os.path.dirname(self.symlink_as)),
                },
            }
            return [d, s]
        else:
            return [d]
