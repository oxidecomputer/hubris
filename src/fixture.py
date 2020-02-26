import cobble
import cobble.project
import cobble.target
import cobble.target.c
import cobble.target.copy_file
import cobble.env
import cobble.ninja_syntax

from itertools import chain
import sys

ENV_DEBUG = True

# Set up project.
project = cobble.project.Project(
    root = 'ROOT',
    build_dir = 'BUILD',
)

project.add_ninja_rules(cobble.target.c.ninja_rules)

kr = cobble.env.KeyRegistry()
kr.define(cobble.env.DEPS_KEY)
for k in cobble.target.KEYS:
    kr.define(k)
for k in cobble.target.c.KEYS:
    kr.define(k)
empty_env = cobble.env.Env(kr, {})

c_env = empty_env.derive({
    'cc': 'gcc',
    'cxx': 'g++',
})

project.define_environment('env', c_env)

package_a = cobble.project.Package(project, 'a')
package_b = cobble.project.Package(project, 'b')

target_lib = cobble.target.c.c_library(package_a, 'barlib',
    sources = ['bar.c'],
)
package_a.add_target(target_lib)

target_t = cobble.target.c.c_binary(package_a, 't',
    env = 'env',
    sources = ['foo.c'],
    deps = [':barlib'],
)
package_a.add_target(target_t)

target_t_opt = cobble.target.c.c_binary(package_a, 't_opt',
    env = 'env',
    sources = ['foo.c'],
    deps = [':barlib'],
    extra = {
        'c_flags': ['-O'],
    },
)
package_a.add_target(target_t_opt)

merged, products = target_t.evaluate(empty_env)
merged2, products2 = target_t_opt.evaluate(empty_env)

products.update(products2)

def pretty_dict(d):
    print("  {")
    for k, v in d.items():
        print("    %r: %r," % (k, v))
    print("  }")

nw = cobble.ninja_syntax.Writer(sys.stdout)
for (relpath, package) in project.packages.items():
    for (name, target) in package.targets.items():
        nw.comment('//' + relpath + ':' + name)
        for item, value in target.stats().items():
            nw.comment(' - %s = %r' % (item, value))

for (name, parts) in project.ninja_rules.items():
    nw.rule(name, **parts)
for (target, env), ps in products.items():
    for p in ps:
        if ENV_DEBUG:
            for k in sorted(p.env):
                nw.comment("%s = %r" % (k, p.env[k]))
        for d in p.ninja_dicts():
            nw.build(**d)
