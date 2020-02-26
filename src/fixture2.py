import cobble
import cobble.project
import cobble.target
import cobble.target.c
import cobble.target.copy_file
import cobble.env
import cobble.ninja_syntax
import cobble.loader

from itertools import chain
import sys

ENV_DEBUG = True

project = cobble.loader.load(
    root = '..',
    build_dir = '.',
)

target_t = project.find_target('//a:t')
#    env = 'env',
#    sources = ['foo.c'],
#    deps = [':barlib'],
#)

target_t_opt = project.find_target('//a:t_opt')
#cobble.target.c.c_binary(package_a, 't_opt',
#    env = 'env',
#    sources = ['foo.c'],
#    deps = [':barlib'],
#    extra = {
#        'c_flags': ['-O'],
#        'c_library_archive_products': True,
#    },
#)

empty_env = cobble.env.Env(cobble.env.KeyRegistry(), {})
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
