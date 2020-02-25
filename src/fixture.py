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

target_t = cobble.target.c.c_binary(package_a, 't',
    env = 'env',
    sources = ['foo.c', 'bar.c'],
)

package_a.add_target(target_t)

merged, products = target_t.evaluate(empty_env)

def pretty_dict(d):
    print("  {")
    for k, v in d.items():
        print("    %r: %r," % (k, v))
    print("  }")

nw = cobble.ninja_syntax.Writer(sys.stdout)
for (name, parts) in project.ninja_rules.items():
    nw.rule(name, **parts)
for (target, env), ps in products.items():
    for p in ps:
        if ENV_DEBUG:
            for k in sorted(p.env):
                nw.comment("%s = %r" % (k, p.env[k]))
        for d in p.ninja_dicts():
            nw.build(**d)
