"""Project-level configuration and state."""

import os.path

class Project(object):
    """A Project tracks the overall build configuration, filesystem paths,
    registered plugins/keys, etc. and provides services that relate to that."""

    def __init__(self, root, build_dir):
        """Creates a Project.

        root: path to root of project structure.
        build_dir: path to build directory.
        """
        self.root = root
        self.build_dir = build_dir

        self.named_envs = {}
        self.packages = {}
        self.ninja_rules = {
            'cobble_symlink_product': {
                'command': 'ln -sf $target $out',
                'description': 'SYMLINK $out',
            },
        }

    # TODO: rename something like static_path?
    def inpath(self, *parts):
        """Creates a path to an input resource within the project tree by
        separating the given path components by the path separator
        character."""
        return os.path.join(self.root, *parts)

    def outpath(self, env, *parts):
        """Creates a path to an output resource within the build directory.

        Output resources are distinguished by their environments; the same
        product may be built several times, in different environments, and
        stored in separate places. Thus, 'outpath' requires the environment to
        be provided.
        """
        return os.path.join(self.build_dir, 'env', env.digest, *parts)

    def linkpath(self, *parts):
        """Creates a path into the 'latest' symlinks in the build directory."""
        return os.path.join(self.build_dir, 'latest', *parts)

    def add_package(self, package):
        assert package.relpath not in self.packages, \
                "duplicate package at %s" % package.relpath
        assert package.project is self, "package project misconfigured"
        self.packages[package.relpath] = package

    def define_environment(self, name, env):
        assert name not in self.named_envs, \
            "more than one environment named %s" % name
        self.named_envs[name] = env

    def add_ninja_rules(self, rules):
        for k, v in rules.items():
            if k in self.ninja_rules:
                assert v == self.ninja_rules[k], \
                        "ninja rule %s defined incompatibly in multiple places" % k
            else:
                self.ninja_rules[k] = v


class Package(object):
    def __init__(self, project, relpath):
        """Creates a Package and registers it with 'project'."""
        self.project = project
        self.relpath = os.path.normpath(relpath)
        self.targets = {}

        project.add_package(self)

    def add_target(self, target):
        assert target.name not in self.targets, \
                "duplicate target %s in package %s" % (target.name, self.relpath)
        self.targets[target.name] = target

    def outpath(self, env, *parts):
        """Creates a path to an output resource within this package."""
        return self.project.outpath(env, self.relpath, *parts)

    def inpath(self, *parts):
        """Creates a path to an input resource within this package."""
        return self.project.inpath(self.relpath, *parts)

    def linkpath(self, *parts):
        """Creates a path into the 'latest' symlinks for this package."""
        return self.project.linkpath(self.relpath, *parts)
