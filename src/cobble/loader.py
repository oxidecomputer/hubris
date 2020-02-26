import importlib

import cobble.env

def load(root, build_dir):
    kr = cobble.env.KeyRegistry()

    for k in cobble.env.KEYS: kr.define(k)
    for k in cobble.target.KEYS: kr.define(k)

    project = cobble.project.Project(root, build_dir)
    packages_to_visit = []
    installed_modules = {}

    def seed(*paths):
        nonlocal packages_to_visit
        packages_to_visit += paths

    def install(module_name):
        nonlocal kr

        module = importlib.import_module(module_name)
        if hasattr(module, 'KEYS'):
            for k in module.KEYS:
                kr.define(k)

        installed_modules[module.__name__] = module

    def environment(name, base = None, contents = {}):
        assert name not in project.named_envs, \
                "More than one environment named %r" % name
        if base:
            assert base in project.named_envs, \
                "Base environment %r does not exist (must appear before)" % base
            base_env = project.named_envs[base]
        else:
            base_env = cobble.env.Env(kr, {
                'ROOT': project.root,
                'BUILD': project.build_dir,
            })

        env = base_env.derive(cobble.env.prepare_delta(contents))
        project.named_envs[name] = env

    # Read in BUILD.conf and eval it for its side effects
    with open(project.inpath('BUILD.conf'), 'r') as f:
        exec(f.read(), {
            'seed': seed,
            'install': install,
            'environment': environment,
        })

    def define_key(name, **kw):
        key = cobble.env.EnvKey(name, **kw)
        kr.define(key)

    while packages_to_visit:
        ident = packages_to_visit.pop()
        relpath = _get_relpath(ident)
        if relpath not in project.packages:
            package = cobble.project.Package(project, relpath)
            pkg_env = {
                'PKG': package.inpath(),
                'define_key': define_key,
            }
            for mod in installed_modules.values():
                if hasattr(mod, 'package_verbs'):
                    for name, fn in mod.package_verbs.items():
                        pkg_env[name] = _wrap_verb(package, fn, packages_to_visit)
                if hasattr(mod, 'global_functions'):
                    for name, fn in mod.global_functions.items():
                        pkg_env[name] = fn

            with open(package.inpath('BUILD'), 'r') as f:
                exec(f.read(), pkg_env)

    for mod in installed_modules.values():
        if hasattr(mod, 'ninja_rules'):
            project.add_ninja_rules(mod.ninja_rules)

    return project

def _wrap_verb(package, verb, packages_to_visit):
    def verb_wrapper(*pos, **kw):
        nonlocal packages_to_visit
        tgt = verb(package, *pos, **kw)
        if tgt:
            package.add_target(tgt)
        # TODO this is where we'd return for extend_when
        packages_to_visit += tgt.deps

    return verb_wrapper
    

def _get_relpath(ident):
    assert ident.startswith('//'), "bogus ident got in: %r" % ident
    return ident[2:].split(':')[0]
