import cobble
import cobble.project
import cobble.target
import cobble.target.c
import cobble.target.copy_file
import cobble.env

from itertools import chain

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

project.define_environment('env', empty_env)

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

print("RULES")
for (name, parts) in project.ninja_rules.items():
    print(name)
    pretty_dict(parts)

print("PRODUCTS")
for (target, env), ps in products.items():
    print("{:20} {:16}".format(target.ident, env.digest))
    for d in chain(*(p.ninja_dicts() for p in ps)):
        pretty_dict(d)
