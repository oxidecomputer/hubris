import os
import cobble.ninja_syntax
from itertools import chain
from collections import defaultdict

def write_ninja_files(project):
    # Write implicit deps file. This ensures that build.ninja gets regenerated
    # if any of the build files change.
    # TODO: I think this might be slightly overcomplicated.
    with open('.build.ninja.deps.tmp', 'w') as f:
        f.write("build.ninja: \\\n")
        for filename in project.files():
            f.write("  %s \\\n" % filename)

        f.write("\n") # for final backslash

    os.rename('.build.ninja.deps.tmp', 'build.ninja.deps')

    # Begin emitting build.ninja

    writer = cobble.ninja_syntax.Writer(open('.build.ninja.tmp', 'w'))

    # Write automatic regeneration rules.
    writer.comment('Automatic regeneration')
    writer.rule(
        name = 'cobble_generate_ninja',
        command = './cobble init --reinit ' + project.root,
        description = '(cobbling something together)',
        depfile = 'build.ninja.deps',
    )

    writer.build(
        outputs = ['build.ninja'],
        rule = 'cobble_generate_ninja',
    )

    writer.newline()

    # Write rules. Sort rules alphabetically by name to make file more
    # predictable.
    ninja_rules = sorted(project.ninja_rules.items(), key = lambda kv: kv[0])
    for name, rule in ninja_rules:
        writer.rule(name = name, **rule)
        writer.newline()

    # Write products. Sort products to make file more predictable.
    # This map winds up having the shape
    #   unique_products_by_target[target_ident][env_digest] = [ninja_dict]
    unique_products_by_target = defaultdict(lambda: {})

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
            flat = list(chain(*(p.ninja_dicts() for p in products)))

            if ed in unique_products_by_target[ti]:
                # This *should* succeed trivially, but... wouldn't we want to
                # find out if it didn't?
                assert unique_products_by_target[ti][ed] == flat, \
                        "internal error: evaluations differ"
            else:
                unique_products_by_target[ti][ed] = flat

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
