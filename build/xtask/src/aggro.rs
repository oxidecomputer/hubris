// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::config::Config;
use anyhow::Result;
use indexmap::IndexMap;
use ordered_toml::Value;
use pulldown_cmark::{Event, HeadingLevel, Tag, TagEnd, html};
use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
    fmt::Write as _,
    fs,
    io::Write as _,
    path::Path,
};
use toml_task::Task;

// Todo: not *everything*? Probably just something fully GitHub
// Flavored Markdown compatible?
const PULLDOWN_OPTS: pulldown_cmark::Options = pulldown_cmark::Options::all();

#[derive(Default, Debug)]
struct TaskMeta {
    calls: HashSet<String>,
    called_by: HashSet<String>,
}

pub fn run(app_toml: &Path, output: Option<&Path>) -> Result<()> {
    let cfg = Config::from_file(app_toml)?;

    println!("{}", std::env::current_dir().unwrap().display());

    use cargo_metadata::MetadataCommand;
    let metadata = MetadataCommand::new()
        .manifest_path("./Cargo.toml")
        .exec()?;

    // Analysis
    let mut meta = HashMap::new();
    for (name, _) in cfg.tasks.iter() {
        meta.insert(name.clone(), TaskMeta::default());
    }
    for (name, task) in cfg.tasks.iter() {
        let tmeta = meta.get_mut(name).unwrap();
        for tst in task.task_slots.values() {
            tmeta.calls.insert(tst.clone());
        }
        for tst in task.task_slots.values() {
            meta.get_mut(tst)
                .unwrap()
                .called_by
                .insert(name.to_string());
        }
    }

    let mut task_docs = vec![];
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

        task_docs.push((name.to_string(), taskdocpath, task));
    }

    // TODO: We probably actually want to bundle up all the content first before providing
    // the prelude, so we can figure out what the table of contents is
    let mut html_buf = prelude(&format!("\"{}\" Aggregate Docs", cfg.name))?;

    // STAGE 1: Document the App
    write_app_info(&cfg, &mut html_buf)?;

    // STAGE 2: Task Header
    write_all_tasks_header(&cfg, &cfg.tasks, &mut html_buf, &meta)?;

    // STAGE 3: Document each task
    task_docs.sort_unstable_by(|a, b| task_sort((&a.0, a.2), (&b.0, b.2)));
    for (name, docpath, task) in task_docs {
        write_task_info(&name, task, docpath.as_deref(), &mut html_buf)?;
    }

    html_buf.push_str(MARKDOWN_FOOTER);

    if let Some(out) = output {
        let mut file = std::fs::File::create(out).unwrap();
        file.write_all(html_buf.as_bytes()).unwrap();
    } else {
        println!("{html_buf}");
    }

    Ok(())
}

fn write_app_info(cfg: &Config, buf: &mut String) -> Result<()> {
    let mut mkdn = String::new();
    writeln!(&mut mkdn, "# Application: \"{}\"", cfg.name)?;
    writeln!(&mut mkdn)?;

    // TODO: Application level docs?

    // Write to HTML.
    let parser = pulldown_cmark::Parser::new_ext(&mkdn, PULLDOWN_OPTS);
    html::push_html(buf, parser);

    if let Some(readme) = cfg.docfile.as_ref() {
        let app_readme = std::fs::read_to_string(readme)?;
        let parser =
            pulldown_cmark::Parser::new_ext(&app_readme, PULLDOWN_OPTS);
        let mut base = readme.to_owned();
        base.pop();
        let stream = parser.map(|evt| touchup(evt, Some(&base)));
        html::push_html(buf, stream);
    } else {
        // Placeholder for no docs!
        //
        // Write this as markdown for laziness, then HTMLify it
        let mut mkdn = String::new();
        writeln!(&mut mkdn, "# \"{}\" docs", cfg.name)?;
        writeln!(&mut mkdn)?;
        writeln!(&mut mkdn, "(this page intentionally left blank)")?;
        writeln!(&mut mkdn)?;

        // Write to HTML.
        let parser = pulldown_cmark::Parser::new_ext(&mkdn, PULLDOWN_OPTS);
        let stream = parser.map(|evt| touchup(evt, None));
        html::push_html(buf, stream);
    }
    Ok(())
}

fn write_all_tasks_header(
    cfg: &Config,
    tasks: &IndexMap<String, Task<Value>>,
    buf: &mut String,
    meta: &HashMap<String, TaskMeta>,
) -> Result<()> {
    // Write this as markdown for laziness, then HTMLify it
    let mut mkdn = String::new();
    writeln!(&mut mkdn, "# \"{}\" Tasks", cfg.name)?;
    writeln!(&mut mkdn)?;
    writeln!(
        &mut mkdn,
        "| task: crate | priority | stack (bytes) | interrupts | client of | server for |"
    )?;
    writeln!(
        &mut mkdn,
        "| :--          | :---     | :---          | :---       | :---  | :---      |"
    )?;
    let mut tasks: Vec<(String, &Task)> =
        tasks.iter().map(|(a, b)| (a.clone(), b)).collect();
    tasks.sort_unstable_by(|a, b| task_sort((&a.0, a.1), (&b.0, b.1)));

    for (name, task) in tasks.iter() {
        let prio = task.priority.to_string();

        let stack = if let Some(amt) = task.stacksize {
            amt.to_string()
        } else {
            "???".to_string()
        };

        let ints: Vec<&str> =
            task.interrupts.keys().map(String::as_str).collect();
        let ints = if !ints.is_empty() {
            ints.join("<br>")
        } else {
            "-".to_string()
        };

        let tmeta = meta.get(name).unwrap();
        let mut calls: Vec<_> = tmeta.calls.iter().cloned().collect();
        calls.sort_unstable();

        let calls = if !calls.is_empty() {
            calls.join("<br>")
        } else {
            "-".to_string()
        };

        let mut called_by: Vec<_> = tmeta.called_by.iter().cloned().collect();
        called_by.sort_unstable();

        let called_by = if !called_by.is_empty() {
            called_by.join("<br>")
        } else {
            "-".to_string()
        };

        writeln!(
            &mut mkdn,
            "| {}<br>`{}` | {} | {} | {} | {} | {} |",
            name, task.name, prio, stack, ints, calls, called_by,
        )?;
    }

    // TODO: What else do we want here? Top level task tables?

    // Write to HTML. We *don't* do touchup, because this is the top level
    let parser = pulldown_cmark::Parser::new_ext(&mkdn, PULLDOWN_OPTS);
    html::push_html(buf, parser);
    Ok(())
}

fn write_task_info(
    name: &str,
    task: &Task,
    docs: Option<&Path>,
    buf: &mut String,
) -> Result<()> {
    let mut mkdn = String::new();
    writeln!(&mut mkdn, "# Task: \"{name}\" (`{}`)", task.name)?;
    writeln!(&mut mkdn)?;

    // TODO: Meta info about the task, before the readme?

    // Write to HTML.
    let parser = pulldown_cmark::Parser::new_ext(&mkdn, PULLDOWN_OPTS);
    html::push_html(buf, parser);

    if let Some(readme) = docs {
        let task_readme = std::fs::read_to_string(readme)?;
        let parser =
            pulldown_cmark::Parser::new_ext(&task_readme, PULLDOWN_OPTS);
        let mut base = readme.to_owned();
        base.pop();
        let stream = parser.map(|evt| touchup(evt, Some(&base)));
        html::push_html(buf, stream);
    } else {
        // Placeholder for no docs!
        //
        // Write this as markdown for laziness, then HTMLify it
        let mut mkdn = String::new();
        writeln!(&mut mkdn, "# `{}` docs", task.name)?;
        writeln!(&mut mkdn)?;
        writeln!(&mut mkdn, "(this page intentionally left blank)")?;
        writeln!(&mut mkdn)?;

        // Write to HTML.
        let parser = pulldown_cmark::Parser::new_ext(&mkdn, PULLDOWN_OPTS);
        let stream = parser.map(|evt| touchup(evt, None));
        html::push_html(buf, stream);
    }
    Ok(())
}

fn touchup<'a>(evt: Event<'a>, _base: Option<&'a Path>) -> Event<'a> {
    match evt {
        Event::Start(tag) => {
            match tag {
                Tag::Link {
                    link_type,
                    dest_url,
                    title,
                    id,
                } => {
                    if !dest_url.starts_with("http") {
                        // TODO: rewrite relative to `https://github.com/oxidecomputer/hubris/blob/master/`?
                        // use _base to figure out relative paths, we might also need to
                        println!(
                            "->{}",
                            _base.unwrap().canonicalize().unwrap().display()
                        );
                        println!("WARN: We should be rewriting {dest_url}!");
                    }
                    Event::Start(Tag::Link {
                        link_type,
                        dest_url,
                        title,
                        id,
                    })
                }
                Tag::Image {
                    link_type,
                    dest_url,
                    title,
                    id,
                } => {
                    if !dest_url.starts_with("http") {
                        // TODO: rewrite relative to `https://github.com/oxidecomputer/hubris/blob/master/`?
                        // use _base to figure out relative paths, we might also need to
                        println!("WARN: We should be rewriting {dest_url}!");
                    }
                    Event::Start(Tag::Image {
                        link_type,
                        dest_url,
                        title,
                        id,
                    })
                }
                // Bump down headings one notch, to allow for top level docs
                Tag::Heading {
                    level,
                    id,
                    classes,
                    attrs,
                } => {
                    let level = match level {
                        HeadingLevel::H1 => HeadingLevel::H2,
                        HeadingLevel::H2 => HeadingLevel::H3,
                        HeadingLevel::H3 => HeadingLevel::H4,
                        HeadingLevel::H4 => HeadingLevel::H5,
                        HeadingLevel::H5 => HeadingLevel::H6,
                        HeadingLevel::H6 => HeadingLevel::H6,
                    };
                    Event::Start(Tag::Heading {
                        level,
                        id,
                        classes,
                        attrs,
                    })
                }

                other => Event::Start(other),
            }
        }
        Event::End(tag_end) => match tag_end {
            TagEnd::Heading(heading_level) => {
                Event::End(TagEnd::Heading(match heading_level {
                    HeadingLevel::H1 => HeadingLevel::H2,
                    HeadingLevel::H2 => HeadingLevel::H3,
                    HeadingLevel::H3 => HeadingLevel::H4,
                    HeadingLevel::H4 => HeadingLevel::H5,
                    HeadingLevel::H5 => HeadingLevel::H6,
                    HeadingLevel::H6 => HeadingLevel::H6,
                }))
            }
            other => Event::End(other),
        },

        other => other,
    }
}

/// Sort by priority (lowest first), then by name
fn task_sort(a: (&str, &Task), b: (&str, &Task)) -> Ordering {
    match a.1.priority.cmp(&b.1.priority) {
        Ordering::Less => Ordering::Less,
        Ordering::Equal => a.0.cmp(b.0),
        Ordering::Greater => Ordering::Greater,
    }
}

// TODO: don't use CDN'd CSS - https://cdnjs.com/libraries/github-markdown-css
// TODO: we need some templating for the title
// From: https://github.com/sindresorhus/github-markdown-css
const PRELUDE_PART_ONE: &str = r#"
<!DOCTYPE html>
<html>
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
"#;

const PRELUDE_PART_TWO: &str = r#"
  <link rel="stylesheet" href="https://cdnjs.cloudflare.com/ajax/libs/github-markdown-css/5.8.1/github-markdown.min.css">
  <style>
      .markdown-body {
          box-sizing: border-box;
          min-width: 200px;
          max-width: 980px;
          margin: 0 auto;
          padding: 45px;
      }

      @media (max-width: 767px) {
          .markdown-body {
              padding: 15px;
          }
      }

      @media (prefers-color-scheme: dark) {
          body {
              background-color: #0d1117;
          }
      }
  </style>
</head>
<body>
  <article class="markdown-body">
"#;

fn prelude(title: &str) -> Result<String> {
    let mut out = String::new();
    out.push_str(PRELUDE_PART_ONE);
    writeln!(&mut out, "  <title>{title}</title>")?;
    out.push_str(PRELUDE_PART_TWO);
    Ok(out)
}

const MARKDOWN_FOOTER: &str = r#"
  </article>
</body>
</html>
"#;

// IDEAS FOR STUFF TO ADD TO THE DOCs:
//
// * A 2d table of all deps, unified across all app+tasks, showing which used
//   * maybe either a checkmark, OR a version number
// * a `<details>` box for the full unified app toml
// * a listing of flash and ram sizes foreach task, maybe in a table?
//   * is there more metadata that would be good to table-ify?
// * the .dot output
//   * use https://crates.io/crates/layout-rs to just render the existing
//     dot syntax we produce? do as an inline svg?
