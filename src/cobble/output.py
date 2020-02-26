import os
import cobble.ninja_syntax
from itertools import chain

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

    # Begin emitting rules.
    for name, rule in project.ninja_rules.items():
        writer.rule(name = name, **rule)
        writer.newline()

    # Begin emitting products.
    # TODO: placeholder_env is necessary only because None doesn't work
    placeholder_env = cobble.env.Env(cobble.env.KeyRegistry(), {})
    unique_products = {}
    for target in project.concrete_targets():
        _topomap, products = target.evaluate(placeholder_env)
        for (p_target, p_env), p_products in products.items():
            # TODO: check for duplicate outputs?
            # TODO: provide stable sort to simplify diffing/debugging
            if p_target.concrete:
                writer.comment('Target: ' + p_target.ident)
            else:
                writer.comment('Target: ' + p_target.ident + ' @ ' + p_env.digest)
            for p in p_products:
                for nd in p.ninja_dicts():
                    writer.build(**nd)
                    writer.newline()

    # TODO uhhh shouldn't the file get closed first?
    os.rename('.build.ninja.tmp', 'build.ninja')
