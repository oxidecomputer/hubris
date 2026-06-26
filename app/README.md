# Applications

This folder contains the top level "Application"s of Hubris. By convention, each
subfolder of `app/` is a "project", and contains:

* A binary Rust project which serves as the boot-time "entry point", responsible
  for hardware initialization, and starting the kernel.
* One or more `.toml` files, which serve as configuration for the Hubris build
  system. These can either be partial re-usable fragments, or complete
  end-configurations, known as "manifests".
* For manifests composed of multiple fragments, the build system is responsible
  for combining the top-level TOML file and any inherited fragments into a
  single combined manifest document, which is shipped with the Hubris archive.

These TOML files contain information such as the tasks that will be built for
the firmware image, as well as information such as pin/interrupt mapping,
RAM/flash range mapping, notifications, and build-time feature enablement.

Each of the end-configurations will all use the common binary Rust project as
the entry point. It is common to have one manifest for each hardware board
revision, or different modes of operation of that hardware, described more
below.

## `.toml` file naming conventions

The following are informal conventions currently followed by the Hubris project.
These are "informal" because they are not special-cased in the Hubris build
system.

### `base.toml`

Often, the majority of configuration for a given project is common across all
hardware revisions and modes of operation. In this case, the maximal common
set of configuration is done in a `base.toml`, to avoid needing to update
multiple manifests when a setting is changed.

This `base.toml` fragment will then be inherited by all of the other manifests.

### `-dev` suffix

`-dev` manifests are typically used during DEVelopment. They often enable
debugging interfaces or options that are not enabled in production releases.

These options may be rolled into a `dev.toml` fragment that is inherited by more
specific `*-dev.toml` manifests.

### `-lab` suffix

`-lab` manifests are used in "benchtop" hardware testing. They typically also
inherit any `-dev` configuration settings. The largest distinction here is that
`-lab` manifests will not automatically perform startup power sequencing steps
done by other firmware images.

These options may be rolled into a `lab.toml` fragment that is inherited by more
specific `*-lab.toml` manifests.

### `-bu` suffix

`-bu` manifests are for "BringUp". This convention is not widely used anymore.

### `-standalone` suffix

`-standalone` manifests are used when a Hubris-based system is expected
to operate without a major component it is designed to support. An example of
this is the `grapefruit` project, which may operate without the `ruby`
development board featuring the AMD processor.

In this configuration, pin mapping may be changes to allow greater access to
I/O ports for testing and verification activities.
