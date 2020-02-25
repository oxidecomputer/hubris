# This is a demo case, and a test for the "make simple things simple" goal of
# the rewrite. Its object is to take a file from the build system and copy it
# to a new location.
#
# The source may be:
# - a relative path to a static file within the same package,
# - an output file of another target,
# - another target, for the case where the target has only one output file
#
# The destination is always a filename within the current package outpath. If
# omitted it defaults to the target name.

from cobble.plugin import *

@target_def
def copy_file(loader, package, name, /, *,
        source: Rewrite,
        dest: Rewrite = None,
        local: Delta = {},
        using: Delta = {},
        ):
    # Function that will be used to compute using delta and product list.
    def using_and_products(env_local):
        # Allow environment interpolation in both source and dest
        e_source = env_local.rewrite(source)
        e_dest = env_local.rewrite(dest) or name

        # Create a product node describing our action.
        product = cobble.Product(
            inputs = [package.inpath(e_source)],
            rule = 'copy_file', # see below
            variables = {
                'short_name': dest,
            },
            # Note: the output path is sensitive to the *entire* contents of
            # env_local. This is wrong. It need only be sensitive to anything
            # that affects the *input.*
            outputs = [package.outpath(env_local, e_dest)],
        )

        # We have no using delta, only a product
        return (None, [product])

    # Create a target node describing the abstract rule.
    return cobble.Target(
        using = using_and_products,
    )

package_verbs = [copy_file]

ninja_rules = {
    'copy_file': {
        'command': 'cp $in $out',
        'description': 'CP $short_name',
    },
}
