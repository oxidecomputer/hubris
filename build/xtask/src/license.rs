// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::{env, fs::File};

use anyhow::Result;
use walkdir::{DirEntry, WalkDir};

const LICENSE_HEADER: &str =
    "// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

";

/// alternative header with //s as bookends
const LICENSE_HEADER_ALT: &str = "//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
";

pub fn check() -> Result<bool> {
    // assume only the best about our codebase
    let mut fail = false;

    // this gets the manifest of the xtask, so we need to go up two levels to
    // actually get the root directory
    let mut root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    root.push("..");
    root.push("..");

    let walker = WalkDir::new(&root).into_iter();

    // don't bother walking the target directory
    let walker = walker.filter_entry(|entry| !is_target_dir(entry));

    for entry in walker {
        let entry = entry?;

        // we are only checking for Rust files for now
        if !is_rust_file(&entry) {
            continue;
        }

        let check_header = |header: &str| -> Result<bool> {
            let f = File::open(entry.path())?;
            let reader = BufReader::with_capacity(header.len(), f);
            for (text, header) in reader.lines().zip(header.lines()) {
                let text = text?;

                if text != header {
                    return Ok(false);
                }
            }

            Ok(true)
        };

        // !check_header(LICENSE_HEADER)? && !check_header(LICENSE_HEADER_ALT)?;

        if !check_header(LICENSE_HEADER)? {
            if !check_header(LICENSE_HEADER_ALT)? {
                fail = true;
                let path = entry.path().strip_prefix(&root)?;
                println!("{}: missing header", path.display());
            }
        }
    }

    Ok(fail)
}

fn is_rust_file(entry: &DirEntry) -> bool {
    entry.path().extension().map(|e| e == "rs").unwrap_or(false)
}

fn is_target_dir(entry: &DirEntry) -> bool {
    entry.file_type().is_dir() && (entry.file_name() == "target")
}
