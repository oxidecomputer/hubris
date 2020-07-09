"""Processing Cobble build graphs into Ninja build files."""

import os
import cobble.ninja_syntax
from itertools import chain
from collections import defaultdict

def write_ninja_files(project,
        dump_environments = False):
    """Processes the build graph rooted at 'project' and produces Ninja files.

    Currently, this produces only a single file, 'build.ninja.'
    """
    writer = cobble.ninja_syntax.Writer(open('.build.ninja.tmp', 'w'))

    # Write automatic regeneration rule.
    writer.comment('Automatic regeneration')
    writer.rule(
        name = 'cobble_generate_ninja',
        command = './cobble init --reinit ' + project.root,
        description = '(cobbling something together)',
    )

    writer.build(
        outputs = ['build.ninja'],
        rule = 'cobble_generate_ninja',
        implicit = project.files(),
    )

    writer.newline()

    # Variables globally available in Ninja rules:
    writer.comment('Global rule variables')
    writer.variable('cobble_project_root', project.inpath())
    writer.newline()

    # Write rules. Sort rules alphabetically by name to make file more
    # predictable.
    writer.comment('Ninja rules provided by plugins')
    ninja_rules = sorted(project.ninja_rules.items(), key = lambda kv: kv[0])
    for name, rule in ninja_rules:
        writer.rule(name = name, **rule)
        writer.newline()

    # Write products. Sort products to make file more predictable.
    # This map winds up having the shape
    #   unique_products_by_target[target_ident][env_digest] = [ninja_dict]
    unique_products_by_target = defaultdict(lambda: {})
    unique_products_by_output = {}
    environments_by_digest = {}

    # First product pass: collect all products, do some light checking.
    for concrete_target in project.concrete_targets():
        # Note that it's okay to just naively evaluate all the concrete
        # targets, even though they likely share significant subgraphs, because
        # of memoization in evaluate.
        _topomap, product_map = concrete_target.evaluate(None)

        # Work through all target output in the transitive graph of this
        # concrete target.
        for (target, env), products in product_map.items():
            ti = target.ident
            ed = env.digest if env is not None else 'top'
            environments_by_digest[ed] = env

            # Collect ninja dicts for each product, filtering out any that we've
            # already done. Products can appear twice because graphs can wind up
            # converging due to environment subsetting.
            flat = []
            for prod in products:
                for ninja_dict in prod.ninja_dicts():
                    output_key = frozenset(ninja_dict['outputs'])
                    prev_dict = unique_products_by_output.get(output_key)
                    if prev_dict is not None:
                        assert prev_dict == ninja_dict, \
                            "Conflicting rules produce outputs %r" % output_key
                    else:
                        unique_products_by_output[output_key] = ninja_dict
                        flat.append(ninja_dict)

            if flat:
                unique_products_by_target[ti][ed] = flat

    # If requested, record environment contents.
    if dump_environments:
        writer.comment('ENVIRONMENT LISTING')
        writer.newline()
        for digest, env in environments_by_digest.items():
            if env is None: continue
            writer.comment('Environment %s' % digest)
            for k, v in env.readout_all().items():
                writer.comment('env[%s][%s] = %r' % (digest, k, v), wrap = False)
            writer.newline()

    # Second product pass: process in sorted order. We sort by target
    # identifier, then by env digest.
    for ti, emap in sorted(unique_products_by_target.items(), key = lambda kv: kv[0]):
        env_count = len(emap)
        # If a target is only evaluated in a single environment, we don't need
        # to print its environment digest.
        if env_count == 1:
            writer.comment('---- target %s' % ti)

        for ed, products in sorted(emap.items(), key = lambda kv: kv[0]):
            # If this target appeared multiple times, note its digest in comments.
            if env_count > 1:
                writer.comment('---- target %s @ %s' % (ti, ed))
            for p in products:
                writer.build(**p)
            writer.newline()

    writer.close()

    os.rename('.build.ninja.tmp', 'build.ninja')
