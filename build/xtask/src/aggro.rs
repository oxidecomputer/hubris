use crate::config::Config;
use anyhow::Result;
use std::{fs, path::Path};

pub fn run(app_toml: &Path, _output: Option<&Path>) -> Result<()> {
    let cfg = Config::from_file(app_toml)?;
    println!("* App Docs:");
    println!("  * {:?}", cfg.docfile);
    println!("* Task Docs:");

    use cargo_metadata::MetadataCommand;
    let metadata = MetadataCommand::new()
        .manifest_path("./Cargo.toml")
        .exec()?;

    for (name, task) in cfg.tasks.iter() {
        // Attempt to autodetect task documentation.
        //
        // Traverse the cargo metadata (inspired by `dist::build_archive`) to
        // find the path to the task manifest.
        let taskdocpath = metadata
            .packages
            .iter()
            .find(|p| p.name == task.name)
            .and_then(|pakidge| {
                // Take the manifest path, and pop off the Cargo.toml part
                let mut buf = pakidge.manifest_path.clone();
                buf.pop();
                let mut matches = vec![];

                // For each file in the folder:
                for path in fs::read_dir(&buf).ok()? {
                    // Make sure it's a real path, and we can do stringy things
                    // with the name
                    let Ok(path) = path else {
                        continue;
                    };
                    let fname = path.file_name();
                    let Some(s) = fname.to_str() else {
                        continue;
                    };

                    // Basically do a case insensitive check that the given file
                    // starts with "readme", and ends with ".md" or ".mkdn", which
                    // are the two extensions we currently use.
                    let lower = s.to_lowercase();
                    if !lower.starts_with("readme") {
                        continue;
                    }
                    if !(lower.ends_with("md") || lower.ends_with("mkdn")) {
                        continue;
                    }
                    let path = path.path();
                    if !path.is_file() {
                        continue;
                    }
                    matches.push(path);
                }

                match matches.as_slice() {
                    // No matches
                    [] => None,
                    // Exactly one match
                    [found] => Some(found.clone()),
                    _ => {
                        panic!("too many readmes");
                    }
                }
            });

        println!("  * {name}: {taskdocpath:?}");
    }

    Ok(())
}
