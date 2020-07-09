"""High-level build targets."""

import types
import cobble.env
from cobble.env import is_delta
from itertools import chain
from functools import reduce
import os.path

# Key used to accumulate implicit dependency edges for Ninja.
IMPLICIT = cobble.env.frozenset_key('__implicit__',
        help = ('Accumulates implicit dependency edges on build products for '
                'use by Ninja.'))

# Key used to accumulate order-only (i.e. not time-sensitive) dependency edges
# for Ninja.
ORDER_ONLY = cobble.env.frozenset_key('__order_only__',
        help = ('Accumulates order-only dependency edges on build products for '
                'use by Ninja. Order-only dependencies only need to *exist*, '
                'rather than needing to be up-to-date.'))

KEYS = frozenset([IMPLICIT, ORDER_ONLY])

class Target(object):
    """A high-level, parameterizable, build target.

    A 'Target' is the entity created by build rules in a BUILD file.

    A 'Target' is later evaluated in a particular environment to produce zero
    or more 'Product's.
    """

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
        concrete: if 'True', this target names its own environment and can be
            built without additional information.
        """
        assert isinstance(name, str)
        if concrete:
            assert isinstance(down, types.FunctionType)
        else:
            assert is_delta(down)
        assert is_delta(local)
        assert isinstance(using_and_products, types.FunctionType)
        assert isinstance(deps, (list, set, frozenset, tuple)) \
                and all(isinstance(d, str) for d in deps)

        # Process relative deps so plugins don't have to
        deps = frozenset(package.make_absolute(d) for d in deps)

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
        """Returns the identifier of this target, of the form that would be
        used in deps.
        """
        rp = self.package.relpath
        if rp == '.':
            # Special case for targets defined at the project root
            return '//:' + self._name
        else:
            return '//' + rp + ':' + self._name

    @property
    def deps(self):
        """Returns the identifiers of targets this target directly depends
        on, as a 'frozenset' of 'str'.
        """
        return self._deps

    @property
    def concrete(self):
        """Checks whether this Target is concrete, i.e. can be built without
        additional context.
        """
        return self._concrete

    @property
    def name(self):
        """Returns the name of this 'Target'."""
        return self._name

    def stats(self):
        """Returns a collection of internal statistics on this 'Target'. The
        result is only valid after build graph processing during build file
        output. The result is a dict, but the set of keys is subject to
        change.
        """
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

    def evaluate(self, env_up):
        """Evaluates this 'Target' in a concrete environment. This is the
        central implementation of build graph processing.

        The result is a pair of '(rank_map, products)'.

        'rank_map' is a dict providing information about the transitive
        dependency graph. Each key is a tuple '(target, env)' (recording each
        unique environment used for any given target), and the values are tuples
        '(rank, using_delta)'. 'rank' measures the length of the longest path
        from this 'Target' ('self') to the evaluation of '(target, env)' within
        this graph; the entry for '(self, env_up)' has rank 0.

        'products' is another dict providing information about concrete build
        products needed for each target evaluation. It uses the same keys as
        'rank_map', tuples of '(target, env)', but the values are lists of
        'Product'.

        The algorithm used by 'evaluate' requires traversing and evaluating the
        entire transitive dependency graph in every relevant environment. To
        keep this from being explosively costly, evaluations are *memoized*, so
        that later calls to 'evaluate' with the same values of 'self' and
        'env_up' return a cached value.
        """
        try:
            if env_up not in self._evaluate_memos:
                self._evaluate_memos[env_up] = RecursionDetector
                self._evaluate_memos[env_up] = self._evaluate(env_up)

            result = self._evaluate_memos[env_up]
            if isinstance(result, Exception):
                raise result

            assert result is not RecursionDetector, \
                    "cycle detected in build graph evaluation: " \
                    + "%s depends on itself" % self.ident
            return result
        except EvaluationError as e:
            self._evaluate_memos[env_up] = e
            e.add_dep(self, env_up)
            raise
        #except Exception as e:
        #    print('heyo: %r' % e)
        #    ee = EvaluationError(e, self, env_up)
        #    self._evaluate_memos[env_up] = ee
        #    raise ee from e

    def _evaluate(self, env_up):
        """Non-memoized implementation of 'evaluate'."""
        env_down = self.derive_down(env_up)
        # Derive a first version of our local environment, which won't contain
        # any using-deltas applied by our deps. We need this to evaluate our
        # deps!
        env_local_0 = self.derive_local(env_down)
        # Rewrite key references in our deps list, producing a concrete deps list.
        deps = env_local_0.rewrite(self._deps)
        # Resolve all the identifiers and evaluate the targets.
        evaluated_deps = [self.package.find_target(id).evaluate(env_down)
                for id in deps]
        # The evaluated_deps list has the shape:
        #  [ ( {(target, env): (rank, using)}, {(target, env): [product]} ) ]

        # Merge the first maps together.
        merged = _topo_merge(m[0] for m in evaluated_deps)
        # Merge the second maps together.
        # TODO: should a target-env pair appear with *different* products
        # lists, this implementation will hide it!
        products = dict(chain(*(m[1].items() for m in evaluated_deps)))

        # Extract all the using-deltas in the order they should be applied.
        topo_merged = _topo_sort(merged)
        dep_usings = (u for (t, e), (r, u) in topo_merged)

        env_local_1 = reduce(lambda e, dlt: e.derive(dlt), dep_usings, env_local_0)

        self._check_local(env_local_1)

        # Generate parameter object for using-and-products
        rank_map = dict((target, (rank, env))
            for ((target, env), (rank, _)) in topo_merged)

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
        """Implementation hook for running user-defined checks on environments.
        This is a Cobble1 feature that isn't fully implemented here yet."""
        pass

class UsingContext(object):
    """Parameter object handed to the 'using_and_products' functions defined by
    custom target types.

    Defines the following attributes:

    - 'env' is the environment in which the target is being evaluated.
    """

    def __init__(self, *, package, env, product_map, rank_map):
        """Creates a 'UsingContext'."""
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
        return (-r, t.ident, e.digest if e else None, u)
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

# Convenient set of keys that should not be conveyed to Ninja as variables,
# because they're conveyed in other ways.
_special_product_keys = frozenset([IMPLICIT.name, ORDER_ONLY.name])

class Product(object):
    """Concrete build product derived from a 'Target'.

    'Product's correspond to Ninja build rules, though a 'Product' can
    translate to more than one rule (a single "primary" rule and zero or more
    "auxiliary" rules).

    Attributes:

    - 'env' is the environment in which the product is valid.
    - 'inputs' is a tuple of build-directory-relative paths to input files.
    - 'outputs' is a tuple of build-directory-relative paths to output files.
    - 'rule' is the name of the primary Ninja rule.
    - 'symlink_as' is either the name of the concrete symlink in 'latest' for
      this product, or 'None'.
    - 'implicit' is a frozenset of build-directory-relative paths to implicit
      dependencies.
    - 'order_only' is a frozenset of build-directory-relative paths to
      order-only dependencies.
    - 'implicit_output' is a tuple of build-directory-relative paths to
      implicit output files.
    - 'variables' is the dict of variables that will be conveyed into the rule
      for interpolation.
    """

    def __init__(self,
            env,
            outputs,
            rule,
            inputs = None,
            implicit = None,
            order_only = None,
            implicit_outputs = None,
            symlink_as = None,
            dyndep = None):
        """Creates a new Product.

        All parameters directly correspond to the attributes documented at
        class-level. The collection parameters ('inputs', 'implicit',
        'order_only', and 'outputs') will be frozen before being stored.

        'implicit' and 'order_only' extend the sets of implicit and order-only
        dependencies (respectively) inside 'env'.
        """
        self.env = env
        self.inputs = cobble.env.freeze(inputs)
        self.rule = rule
        self.outputs = cobble.env.freeze(outputs)
        self.symlink_as = symlink_as
        self.dyndep = dyndep
        self.implicit_outputs = cobble.env.freeze(implicit_outputs)

        self.implicit = env[IMPLICIT.name]
        if implicit: self.implicit |= frozenset(implicit)
        self.order_only = env[ORDER_ONLY.name]
        if order_only: self.order_only |= frozenset(order_only)

        self.variables = env.without(_special_product_keys).readout_all()

        self._exposed_outputs = {}

    def expose(self, *, path, name):
        """Mark an output of this rule as "exposed," i.e. available to be used
        as a source for other build rules.

        'path' is the build-directory-relative path of the output, which must
        be present in the 'outputs' set given at construction.

        'name' is the name exposed to other rules.
        """
        assert path in self.outputs, \
                "Can't expose path %r that is not in outputs: %r" \
                % (path, self.outputs)
        assert name not in self._exposed_outputs, \
                "Duplicate exposed output name %r" % name
        self._exposed_outputs[name] = path

    def find_output(self, name):
        """Locates the exposed output named 'name', or 'None' if no such
        exposed output exists."""
        return self._exposed_outputs.get(name)

    def exposed_outputs(self):
        """Returns a dict mapping exposed output names to output paths."""
        # defensive copy :-(
        return dict(self._exposed_outputs)

    def ninja_dicts(self):
        """Produces one or more Ninja build rules in dict format, where each
        dict key corresponds to one parameter in the Ninja build rule
        format."""
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
        if self.dyndep: d['dyndep'] = self.dyndep

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

class RecursionDetector:
    pass

class EvaluationError(Exception):
    def __init__(self, cause, target, env):
        self.cause = cause
        self.targets = [(target, env)]

    def add_dep(self, target, env):
        self.targets.append((target, env))
