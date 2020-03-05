"""Project loader for reading BUILD and BUILD.conf files."""

import importlib
import sys
import traceback

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
    def _build_conf_define_key(name, *, type):
        if type == 'string':
            key = cobble.env.overrideable_string_key(name)
        elif type == 'bool':
            key = cobble.env.overrideable_bool_key(name)
        else:
            raise Exception('Unknown key type: %r' % type)
        kr.define(key)

    # Function that will be exposed to BUILD.conf files as 'plugin_path()'
    def _build_conf_plugin_path(*paths):
        sys.path += [project.inpath(p) for p in paths]

    # Read in BUILD.conf and eval it for its side effects
    _compile_and_exec(
        path = project.inpath('BUILD.conf'),
        kind = 'BUILD.conf file',
        globals = {
            # Block access to builtins. TODO: this might be too aggressive.
            '__builtins__': {},

            'seed': _build_conf_seed,
            'install': _build_conf_install,
            'environment': _build_conf_environment,
            'define_key': _build_conf_define_key,
            'plugin_path': _build_conf_plugin_path,

            'ROOT': project.root,
            'BUILD': project.build_dir,
        },
    )

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
            # Block access to builtins. TODO: this might be too aggressive.
            '__builtins__': {},

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
        _compile_and_exec(
            path = package.inpath('BUILD'),
            kind = 'BUILD file',
            globals = pkg_env,
        )

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

class BuildError(Exception):
    """Exception raised if processing of a BUILD/BUILD.conf file fails."""

    def __init__(self, exc_info, kind, path, limit):
        """Creates a BuildError.

        'exc_info' is the information on the exception as received from
        'sys.exc_info()`.

        'kind' is a human-readable str description of what we were processing.

        'path' is a path to the file being processed.

        'limit' is the depth of the traceback that is relevant to the user
        error, i.e. does not include Cobble stack frames.
        """
        self.exc_info = exc_info
        self.kind = kind
        self.path = path
        self.limit = limit

def _compile_and_exec(path, kind, globals):
    """Implementation factor of BUILD and BUILD.conf evaluation. Loads the file
    at 'path' and execs it in an environment of 'globals', reporting the
    failure as 'kind' if it occurs."""
    with open(path, 'r') as f:
        try:
            mod = compile(
                source = f.read(),
                filename = path,
                mode = 'exec',
                dont_inherit = 1,
            )
            exec(mod, globals)
        except:
            exc_info = sys.exc_info()
            limit = len(traceback.extract_tb(exc_info[2])) - 1
            raise BuildError(
                    exc_info = exc_info,
                    limit = limit,
                    kind = kind,
                    path = path) from exc_info[1]

