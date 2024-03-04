# Scripts

Scripts to aid in Hubris development.

## build-all

Build using all supported app.toml files.
The "unsupported" app.toml files are those that are broken and should be
fixed but do not currently impact our production work.

```
    Usage: build-all [options] [args]
      -c # Continue to next app.toml on build errors
      -h # Help (this message)
      -n # No action, just list app.toml files to process.
      -r # Remove previous log files (rebuild everything)
      -u # Attempt to build including unsupported app.toml files
    Run "cargo xtask $args app.toml" for every available app.toml
    $args defaults to "dist"
```
