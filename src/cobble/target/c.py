import cobble.env
import cobble.target
import os.path
from itertools import chain

DEPS_INCLUDE_SYSTEM = cobble.env.EnvKey(
    name = 'c_deps_include_system',
    default = True,
    from_literal = lambda x: bool(x),
    readout = lambda x: '-MMD' if x else '-MD',
)

LINK_SRCS = cobble.env.appending_string_seq_key('c_link_srcs')
LINK_FLAGS = cobble.env.appending_string_seq_key('c_link_flags')
CC = cobble.env.overrideable_string_key('cc')
CXX = cobble.env.overrideable_string_key('cxx')

KEYS = frozenset([DEPS_INCLUDE_SYSTEM, LINK_SRCS, LINK_FLAGS, CC, CXX])

_common_keys = frozenset([cobble.target.ORDER_ONLY.name, cobble.target.IMPLICIT.name])
_compile_keys = _common_keys | frozenset([DEPS_INCLUDE_SYSTEM.name])
_link_keys = _common_keys | frozenset([CXX.name, LINK_SRCS.name,
    LINK_FLAGS.name])

def c_binary(package, name, /, *,
        env,
        deps = [],
        sources = [],
        local = {},
        extra = {}):
    
    extra = cobble.env.prepare_delta(extra)
    local = cobble.env.prepare_delta(local)

    def mkusing(env_local):
        # Allow environment key interpolation in source names
        sources_i = env_local.rewrite(sources)
        # Generate object file products for all sources.
        objects = [_compile_object(package, s, env_local) for s in sources]
        # Extract just the output paths
        obj_files = list(chain(*[prod.outputs for prod in objects]))
        # Prepend them to the list of C objects to link in.
        # This assumes that c_link_srcs knows to prepend.
        program_env = env_local.derive({
            LINK_SRCS.name: obj_files,
        })

        # Construct the linked program product in its canonical location.
        program_path = package.outpath(program_env, name)
        program = cobble.target.Product(
            env = program_env.subset(_link_keys),
            outputs = [package.outpath(program_env, name)],
            rule = 'link_c_program',
            inputs = obj_files,
            symlink_as = package.linkpath(name),
        )

        # TODO: this is really just a way of naming the most derived node in
        # the build graph we just emitted, so that our users can depend on just
        # it. This could be factored out.
        using = {
            '__implicit__': program.symlink_as,
        }

        products = objects + [program]
        return (using, products)
        
    return cobble.target.Target(
        package = package,
        name = name,
        concrete = True,
        down = lambda _up_unused: package.project.named_envs[env].derive(extra),
        using_and_products = mkusing,
        local = local,
    )

_file_type_map = {
    '.c': ('compile_c_obj', ['cc', 'c_flags']),
    '.cc': ('compile_cxx_obj', ['cxx', 'cxx_flags']),
    '.cpp': ('compile_cxx_obj', ['cxx', 'cxx_flags']),
    '.S': ('assemble_obj_pp', ['aspp', 'aspp_flags']),
}

# Common factor of targets that compile C code.
def _compile_object(package, source, env):
    ext = os.path.splitext(source)[1]
    rule, keys = _file_type_map[ext]
    # add in the global compile keys
    keys = _compile_keys | frozenset(keys)

    o_env = env.subset(keys)
    return cobble.target.Product(
        env = o_env,
        outputs = [package.outpath(o_env, source + '.o')],
        rule = rule,
        inputs = [package.inpath(source)]
    )

ninja_rules = {
    'compile_c_obj': {
        'command': '$cc $c_deps_include_system -MF $depfile $c_flags -c -o $out $in',
        'description': 'C $in',
        'depfile': '$out.d',
        'deps': 'gcc',
    },
    'link_c_program': {
        'command': '$cxx $c_link_flags, -o $out $in $c_link_srcs',
        'description': 'LINK $out',
    },
}
