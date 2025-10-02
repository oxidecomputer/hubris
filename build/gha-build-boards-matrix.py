#!/usr/bin/env python3
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

# Script to discover all app.toml files to build in the repository. It is written as a Python script
# since it needs to be executed almost instantly (it will run in its own job in GHA other jobs), and
# Python has a TOML parser in its standard library.

from dataclasses import dataclass
from typing import Optional

import sys
import argparse
import json
import os
import subprocess
import tomllib


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("filter_directories", nargs="*")
    parser.add_argument("--write-github-output", action="store_true")
    args = parser.parse_args()

    matrix = []
    for file in files_in_git(args.filter_directories):
        if not file.endswith(".toml"):
            continue

        config = AppToml()
        config.process_file(file)

        if not config.looks_like_app_toml:
            continue

        # If a file was detected as a valid app.toml but doesn't have a name associated with it,
        # it's an intermediate file inherited by other files. Don't add it to the matrix.
        if config.name is None:
            continue

        if config.image_names is None:
            config.image_names = ["default"]

        matrix.append(
            {
                "job_name": build_job_name(file, config),
                "app_name": config.name,
                "app_toml": file,
                # This is tab-separated to help bash split it in a for loop.
                "image": "\t".join(config.image_names),
            }
        )

    # Consistency checks to prevent starting an invalid matrix.
    validate_matrix(matrix)

    if args.write_github_output:
        with open(os.environ["GITHUB_OUTPUT"], "a") as f:
            f.write(f"matrix={json.dumps(matrix)}")
    else:
        print(json.dumps(matrix, indent=4))


def build_job_name(file, config):
    # Temporary hack: some of the jobs are hardcoded in the GitHub branch protection. Until the
    # branch protection is reworked, report the old job names for these jobs.
    if file == "app/cosmo/rev-a.toml":
        return "build-cosmo (cosmo-a) / cosmo-a/app/cosmo/rev-a.toml"
    elif file == "app/gimlet/rev-f.toml":
        return "build-gimlet (gimlet-f) / gimlet-f/app/gimlet/rev-f.toml"
    elif file == "app/psc/rev-c.toml":
        return "build-psc (psc-c) / psc-c/app/psc/rev-c.toml"
    elif file == "app/oxide-rot-1/app.toml":
        return "build-rot (oxide-rot-1) / oxide-rot-1/app/oxide-rot-1/app.toml"
    elif file == "app/sidecar/rev-d.toml":
        return "build-sidecar (sidecar-d) / sidecar-d/app/sidecar/rev-d.toml"

    return config.name


def validate_matrix(matrix):
    errors = []

    # The app name is included in the name of uploaded artifacts, and if there are two jobs with the
    # same name there will be duplicate artifacts, which are rejected by GitHub Actions.
    #
    # If we don't do a consistency check here GitHub Actions will fail the build, but it will only
    # say that it cannot upload an artifact with the same name as an existing artifact. Here in the
    # script we can instead provide a better error message, presenting to the user the paths to both
    # files containing the duplicate app name.
    tomls = {}
    for job in matrix:
        name = job["app_name"]
        toml = job["app_toml"]
        if name in tomls:
            errors.append(f"app {name} is defined in both {tomls[name]} and {toml}")
        else:
            tomls[name] = toml

    if errors:
        for error in errors:
            print(f"error: {error}", file=sys.stderr)
        exit(1)


# Load an Hubris app.toml from disk, transparently handling inheritance.
#
# As we only care about two top-level keys, we don't have to implement support for merging maps.
# Instead we can just do a depth-first loading of all the files, and ignore keys we already found.
@dataclass
class AppToml:
    looks_like_app_toml: bool = False

    name: Optional[str] = None
    image_names: Optional[list[str]] = None

    def merge_content(self, toml):
        # Note that this function is called first on the actual file we are trying to parse, and
        # then for each of the files it inherits from (recursively). We must only store a value if
        # it was not already stored earlier by another file.

        if self.name is None and "name" in toml:
            self.name = toml["name"]
        if self.image_names is None and "image-names" in toml:
            self.image_names = toml["image-names"]

        # Check whether this configuration file looks like an app.toml, instead of another TOML file
        # like a Cargo.toml. We do so by checking whether a top-level `kernel` key is present.
        if "kernel" in toml:
            self.looks_like_app_toml = True

    def process_file(self, path):
        toml = tomllib.loads(open(path).read())
        self.merge_content(toml)

        # Process inheritance, as some of the keys we care about might be in the inherited files.
        if "inherit" in toml:
            base = os.path.dirname(path)
            if isinstance(toml["inherit"], str):
                self.process_file(os.path.join(base, toml["inherit"]))
            elif isinstance(toml["inherit"], list):
                for child in toml["inherit"]:
                    self.process_file(os.path.join(base, child))


def files_in_git(filter_directories):
    # `git ls-files` automatically handles gitignore parsing for us, and is easier to do compared to
    # recursively walking directories in Python. The -z flag delimits files with null bytes rather
    # than newlines, to avoid misbehaviors if some cursed script creates a path containing newlines.
    result = subprocess.run(
        ["git", "ls-files", "-z", *filter_directories],
        text=True,
        check=True,
        stdout=subprocess.PIPE,
    )
    return result.stdout.split("\0")


if __name__ == "__main__":
    main()
