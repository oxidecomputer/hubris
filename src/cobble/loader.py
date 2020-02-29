"""Project loader for reading BUILD and BUILD.conf files."""

import importlib

import cobble.env

def load(root, build_dir):
    """Loads a Project, given the paths to the project root and build output
    directory."""

    # Create a key registry initialized with keys defined internally to Cobble.
    kr = cobble.env.KeyRegistry()
    for k in cobble.target.KEYS: kr.define(k)

    # Create working data structures.
    project = cobble.project.Project(root, build_dir)
    packages_to_visit = []
    installed_modules = {}

    # Function that will be exposed to BUILD.conf files as 'seed()'
    def _build_conf_seed(*paths):
        nonlocal packages_to_visit
        packages_to_visit += paths

    # Function that will be exposed to BUILD.conf files as 'install()'
    def _build_conf_install(module_name):
        nonlocal kr

        module = importlib.import_module(module_name)
        if hasattr(module, 'KEYS'):
            for k in module.KEYS:
                kr.define(k)

        installed_modules[module.__name__] = module

    # Function that will be exposed to BUILD.conf files as 'environment()'
    def _build_conf_environment(name, base = None, contents = {}):
        assert name not in project.named_envs, \
                "More than one environment named %r" % name
        if base:
            assert base in project.named_envs, \
                "Base environment %r does not exist (must appear before)" \
                % base
            base_env = project.named_envs[base]
        else:
            base_env = cobble.env.Env(kr, {})

        env = base_env.derive(cobble.env.prepare_delta(contents))
        project.named_envs[name] = env

    # Function that will be exposed to BUILD.conf files as 'define_key()'
    def _build_conf_define_key(name, /, *, type):
        if type == 'string':
            key = cobble.env.overrideable_string_key(name)
        elif type == 'bool':
            key = cobble.env.overrideable_bool_key(name)
        else:
            raise Exception('Unknown key type: %r' % type)
        kr.define(key)

    # Read in BUILD.conf and eval it for its side effects
    with open(project.inpath('BUILD.conf'), 'r') as f:
        exec(f.read(), {
            'seed': _build_conf_seed,
            'install': _build_conf_install,
            'environment': _build_conf_environment,
            'define_key': _build_conf_define_key,
            'ROOT': project.root,
            'BUILD': project.build_dir,
        })

    # Process the package worklist. We're also extending the worklist in this
    # algorithm, treating it like a stack (rather than a queue). This means the
    # order of package processing is a little hard to predict. Because packages
    # can define keys that have effects on other packages, this should probably
    # get fixed (TODO).
    while packages_to_visit:
        ident = packages_to_visit.pop()

        # Check if we've done this one.
        relpath = _get_relpath(ident)
        if relpath in project.packages:
            continue

        package = cobble.project.Package(project, relpath)
        # Prepare the global environment for eval-ing the package. We provide
        # a few variables by default:
        pkg_env = {
            # Easy access to the path from the build dir to the package
            'PKG': package.inpath(),
            # Easy access to the path from the build dir to the project
            'ROOT': project.root,
            # Location of the build dir
            'BUILD': project.build_dir,

            'define_key': _build_conf_define_key,
        }
        # The rest of the variables are provided by items registered in
        # plugins.
        for mod in installed_modules.values():
            if hasattr(mod, 'package_verbs'):
                for name, fn in mod.package_verbs.items():
                    pkg_env[name] = _wrap_verb(package, fn, packages_to_visit)
            if hasattr(mod, 'global_functions'):
                for name, fn in mod.global_functions.items():
                    pkg_env[name] = fn

        # And now, the evaluation!
        with open(package.inpath('BUILD'), 'r') as f:
            exec(f.read(), pkg_env)

    # Register all plugins' ninja rules. We could probably do this earlier, but
    # hey.
    for mod in installed_modules.values():
        if hasattr(mod, 'ninja_rules'):
            project.add_ninja_rules(mod.ninja_rules)

    return project

def _wrap_verb(package, verb, packages_to_visit):
    """Instruments a package-verb function 'verb' from 'package' with code to
    register the resulting target and scan deps to discover new packages.

    'packages_to_visit' is a reference to a (mutable) list containing relpaths
    we should visit. The function returned from '_wrap_verb' will append
    relpaths of deps to that list. Some of them will be redundant; the worklist
    processing code is expected to deal with this.
    """
    def verb_wrapper(*pos, **kw):
        nonlocal packages_to_visit
        tgt = verb(package, *pos, **kw)
        if tgt:
            package.add_target(tgt)
        # TODO this is where we'd return for extend_when
        packages_to_visit += tgt.deps

    return verb_wrapper

def _get_relpath(ident):
    """Extracts the relative path from the project root to the directory
    containing the BUILD file defining a target named by an ident."""
    assert ident.startswith('//'), "bogus ident got in: %r" % ident
    return ident[2:].split(':')[0]
