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
        if concrete:
            assert isinstance(down, types.FunctionType)
        else:
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
        self._transparent = True

    @property
    def ident(self):
        return '//' + self.package.relpath + ':' + self._name

    @property
    def deps(self):
        return self._deps

    @property
    def concrete(self):
        return self._concrete

    def stats(self):
        return {
            'unique_environments': len(self._evaluate_memos),
        }

    def derive_down(self, env):
        """Derives the down-environment, that is, the environment seen by
        dependencies of this target."""
        if self.concrete:
            # Concrete targets use function-deltas to entirely replace the
            # provided environment. Since we checked that in the constructor,
            # we'll just call the delta directly. This allows concrete targets
            # to be given None as an environment.
            return self._down(env)
        else:
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

        # Generate parameter object for using-and-products
        by_rank = sorted(((target, (rank, env))
            for ((target, env), (rank, _)) in merged.items()),
            key = lambda item: -item[1][0])
        rank_map = dict(by_rank)
        upctx = UsingContext(
            package = self.package,
            env = env_local_1,
            product_map = products,
            rank_map = rank_map
        )

        our_using, our_products = self._using_and_products(upctx)

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

class UsingContext(object):
    def __init__(self, *, package, env, product_map, rank_map):
        self._package = package
        self.env = env
        self._product_map = product_map
        self._rank_map = rank_map

    def rewrite_sources(self, sources):
        """Processes a list of source files and handles interpolation of
        environment keys, and references to outputs of other targets.

        Returns a list of concrete paths.
        """
        result = []
        for s in sources:
            if (s.startswith(':') or s.startswith('//')) and "#" in s:
                ident, output_name = s.split('#')
                target = self._package.find_target(ident)
                rank, target_env = self._rank_map[target]

                out = None
                for p in self._product_map[(target, target_env)]:
                    out = p.find_output(output_name)
                    if out is not None:
                        break

                if out is not None:
                    result += [out]
                else:
                    raise Exception('output %r not found in target %s' % (
                        output_name, ident))
            else:
                result += [self._package.inpath(self.env.rewrite(s))]
        return result


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

        self._exposed_outputs = {}

    def expose(self, *, path, name):
        assert path in self.outputs, \
                "Can't expose path %r that is not in outputs: %r" \
                % (path, self.outputs)
        assert name not in self._exposed_outputs, \
                "Duplicate exposed output name %r" % name
        self._exposed_outputs[name] = path

    def find_output(self, name):
        return self._exposed_outputs.get(name)

    def exposed_outputs(self):
        # defensive copy :-(
        return dict(self._exposed_outputs)

    def ninja_dicts(self):
        # Note: can't sort the outputs or inputs here, because some targets may
        # depend on their order.
        d = {
            'outputs': self.outputs,
            'rule': self.rule,
        }
        if self.inputs: d['inputs'] = self.inputs
        if self.implicit: d['implicit'] = sorted(self.implicit)
        if self.order_only: d['order_only'] = sorted(self.order_only)
        if self.variables: d['variables'] = dict(sorted(self.variables.items()))

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
