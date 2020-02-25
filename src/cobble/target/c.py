import cobble.env
import cobble.target
import os.path
from itertools import chain

DEPS_INCLUDE_SYSTEM = cobble.env.EnvKey(
    name = 'c_deps_include_system',
    default = False,
    from_literal = lambda x: bool(x),
    readout = lambda x: '-MMD' if x else '-MD',
)

LINK_SRCS = cobble.env.prepending_string_seq_key('c_link_srcs')
LINK_FLAGS = cobble.env.appending_string_seq_key('c_link_flags')
CC = cobble.env.overrideable_string_key('cc')
CXX = cobble.env.overrideable_string_key('cxx')
ASPP = cobble.env.overrideable_string_key('aspp')
C_FLAGS = cobble.env.appending_string_seq_key('c_flags')
CXX_FLAGS = cobble.env.appending_string_seq_key('cxx_flags')
ASPP_FLAGS = cobble.env.appending_string_seq_key('aspp_flags')

KEYS = frozenset([DEPS_INCLUDE_SYSTEM, LINK_SRCS, LINK_FLAGS, CC, CXX, C_FLAGS,
    CXX_FLAGS, ASPP, ASPP_FLAGS])

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
        # Construct the linked program product in its canonical location.
        program_env = env_local.subset_require(_link_keys)
        program = cobble.target.Product(
            env = program_env,
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
    '.c': ('compile_c_obj', [CC.name, C_FLAGS.name]),
    '.cc': ('compile_cxx_obj', [CXX.name, CXX_FLAGS.name]),
    '.cpp': ('compile_cxx_obj', [CXX.name, CXX_FLAGS.name]),
    '.S': ('assemble_obj_pp', [ASPP.name, ASPP_FLAGS.name]),
}

# Common factor of targets that compile C code.
def _compile_object(package, source, env):
    ext = os.path.splitext(source)[1]
    rule, keys = _file_type_map[ext]
    # add in the global compile keys
    keys = _compile_keys | frozenset(keys)

    o_env = env.subset_require(keys)
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
        'command': '$cxx $c_link_flags -o $out $in $c_link_srcs',
        'description': 'LINK $out',
    },
}
