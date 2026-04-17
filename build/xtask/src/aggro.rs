use crate::config::Config;
use anyhow::Result;
use indexmap::IndexMap;
use pulldown_cmark::{Event, HeadingLevel, Tag, TagEnd, html};
use ordered_toml::Value;
use std::{fmt::Write as _, fs, io::Write as _, path::Path};
use toml_task::Task;

// Todo: not *everything*? Probably just something fully GitHub
// Flavored Markdown compatible?
const PULLDOWN_OPTS: pulldown_cmark::Options = pulldown_cmark::Options::all();

pub fn run(app_toml: &Path, output: Option<&Path>) -> Result<()> {
    let cfg = Config::from_file(app_toml)?;
    println!("* App Docs:");
    println!("  * {:?}", cfg.docfile);
    println!("* Task Docs:");

    use cargo_metadata::MetadataCommand;
    let metadata = MetadataCommand::new()
        .manifest_path("./Cargo.toml")
        .exec()?;

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

    for (name, docpath, _task) in task_docs.iter() {
        println!("  * {name}: {docpath:?}");
    }

    // TODO: We probably actually want to bundle up all the content first before providing
    // the prelude, so we can figure out what the table of contents is
    let mut html_buf = prelude(&format!("\"{}\" Aggregate Docs", cfg.name))?;

    // STAGE 1: App Header
    write_app_header(&cfg, &mut html_buf)?;

    // STAGE 2: Document the App
    write_app_info(&cfg, &mut html_buf)?;

    // STAGE 3: Task Header
    write_task_header(&cfg, &cfg.tasks, &mut html_buf)?;

    // STAGE 4: Document each task
    for (_name, docpath, task) in task_docs {
        write_task_info(task, docpath.as_deref(), &mut html_buf)?;
    }

    html_buf.push_str(MARKDOWN_FOOTER);

    // TODO: Don't print
    println!("---");
    println!("{html_buf}");
    println!("---");

    if let Some(out) = output {
        let mut file = std::fs::File::create(out).unwrap();
        file.write_all(html_buf.as_bytes()).unwrap();
    }

    Ok(())
}

fn write_app_header(cfg: &Config, buf: &mut String) -> Result<()> {
    // Write this as markdown for laziness, then HTMLify it
    let mut mkdn = String::new();
    writeln!(&mut mkdn, "# \"{}\" Application", cfg.name)?;

    // TODO: What else do we want here? Stuff about the app, not yet the docs?
    struct IoWrite(String);
    impl std::io::Write for IoWrite {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let Ok(s) = std::str::from_utf8(buf) else {
                return Err(std::io::Error::other("not utf-8?"));
            };
            self.0.push_str(s);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut dotout = IoWrite(String::new());
    crate::graph::task_graph_inner(&cfg.app_toml_path, &mut dotout)?;

    // TODO: the `layout` crate doesn't handle comments, filter these
    let mut filtered = String::new();
    for line in dotout.0.lines() {
        if line.trim_start().starts_with("#") {
            continue;
        }
        filtered.push_str(line);
        filtered.push_str("\n");
    }

    let mut parser = layout::gv::DotParser::new(&filtered);
    let graph = match parser.process() {
        Ok(g) => g,
        Err(e) => anyhow::bail!("Graphing error: '{e}'"),
    };
    let mut gb = layout::gv::GraphBuilder::new();
    gb.visit_graph(&graph);
    let mut vg = gb.get();
    let mut svg = layout::backends::svg::SVGWriter::new();
    vg.do_it(false, false, false, &mut svg);
    let content = svg.finalize();

    let mut file = std::fs::File::create("/tmp/layout.svg")?;
    file.write_all(content.as_bytes())?;
    file.flush()?;
    drop(file);

    writeln!(buf, "<svg>")?;
    buf.push_str(&content);
    writeln!(buf, "</svg>")?;


    // Write to HTML. We *don't* do touchup, because this is the top level
    let parser = pulldown_cmark::Parser::new_ext(&mkdn, PULLDOWN_OPTS);
    html::push_html(buf, parser);
    Ok(())
}

fn write_app_info(cfg: &Config, buf: &mut String) -> Result<()> {
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
        writeln!(&mut mkdn, "# \"{}\" Firmware", cfg.name)?;
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

fn write_task_header(
    cfg: &Config,
    tasks: &IndexMap<String, Task<Value>>,
    buf: &mut String,
) -> Result<()> {
    // Write this as markdown for laziness, then HTMLify it
    let mut mkdn = String::new();
    writeln!(&mut mkdn, "# \"{}\" Tasks", cfg.name)?;
    writeln!(&mut mkdn)?;
    writeln!(&mut mkdn, "| task | stack (bytes) | interrupts | task slots |")?;
    writeln!(&mut mkdn, "| :--  | :---          | :---       | :---       |")?;
    let mut tasks: Vec<&Task> = tasks.values().collect();
    tasks.sort_unstable_by_key(|t| &t.name);

    for task in tasks.iter() {
        let stack = if let Some(amt) = task.stacksize {
            amt.to_string()
        } else {
            "???".to_string()
        };

        let ints: Vec<&str> = task.interrupts.keys().map(String::as_str).collect();
        let ints = if !ints.is_empty() {
            ints.join(", ")
        } else {
            "-".to_string()
        };

        let slots: Vec<&str> = task.task_slots.keys().map(String::as_str).collect();
        let slots = if !slots.is_empty() {
            slots.join(", ")
        } else {
            "-".to_string()
        };

        writeln!(&mut mkdn, "| {} | {} | {} | {} |", task.name, stack, ints, slots)?;
    }

    // TODO: What else do we want here? Top level task tables?

    // Write to HTML. We *don't* do touchup, because this is the top level
    let parser = pulldown_cmark::Parser::new_ext(&mkdn, PULLDOWN_OPTS);
    html::push_html(buf, parser);
    Ok(())
}

fn write_task_info(
    task: &Task,
    docs: Option<&Path>,
    buf: &mut String,
) -> Result<()> {
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
        writeln!(&mut mkdn, "# \"{}\" Task", task.name)?;
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
                // pulldown_cmark::Tag::Paragraph => todo!(),
                // pulldown_cmark::Tag::BlockQuote(block_quote_kind) => todo!(),
                // pulldown_cmark::Tag::CodeBlock(code_block_kind) => todo!(),
                // pulldown_cmark::Tag::HtmlBlock => todo!(),
                // pulldown_cmark::Tag::List(_) => todo!(),
                // pulldown_cmark::Tag::Item => todo!(),
                // pulldown_cmark::Tag::FootnoteDefinition(cow_str) => todo!(),
                // pulldown_cmark::Tag::DefinitionList => todo!(),
                // pulldown_cmark::Tag::DefinitionListTitle => todo!(),
                // pulldown_cmark::Tag::DefinitionListDefinition => todo!(),
                // pulldown_cmark::Tag::Table(alignments) => todo!(),
                // pulldown_cmark::Tag::TableHead => todo!(),
                // pulldown_cmark::Tag::TableRow => todo!(),
                // pulldown_cmark::Tag::TableCell => todo!(),
                // pulldown_cmark::Tag::Emphasis => todo!(),
                // pulldown_cmark::Tag::Strong => todo!(),
                // pulldown_cmark::Tag::Strikethrough => todo!(),
                // pulldown_cmark::Tag::Superscript => todo!(),
                // pulldown_cmark::Tag::Subscript => todo!(),
                // pulldown_cmark::Tag::MetadataBlock(metadata_block_kind) => todo!(),
            }
        }
        Event::End(tag_end) => {
            match tag_end {
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
                // pulldown_cmark::TagEnd::Paragraph => todo!(),
                // pulldown_cmark::TagEnd::BlockQuote(block_quote_kind) => todo!(),
                // pulldown_cmark::TagEnd::CodeBlock => todo!(),
                // pulldown_cmark::TagEnd::HtmlBlock => todo!(),
                // pulldown_cmark::TagEnd::List(_) => todo!(),
                // pulldown_cmark::TagEnd::Item => todo!(),
                // pulldown_cmark::TagEnd::FootnoteDefinition => todo!(),
                // pulldown_cmark::TagEnd::DefinitionList => todo!(),
                // pulldown_cmark::TagEnd::DefinitionListTitle => todo!(),
                // pulldown_cmark::TagEnd::DefinitionListDefinition => todo!(),
                // pulldown_cmark::TagEnd::Table => todo!(),
                // pulldown_cmark::TagEnd::TableHead => todo!(),
                // pulldown_cmark::TagEnd::TableRow => todo!(),
                // pulldown_cmark::TagEnd::TableCell => todo!(),
                // pulldown_cmark::TagEnd::Emphasis => todo!(),
                // pulldown_cmark::TagEnd::Strong => todo!(),
                // pulldown_cmark::TagEnd::Strikethrough => todo!(),
                // pulldown_cmark::TagEnd::Superscript => todo!(),
                // pulldown_cmark::TagEnd::Subscript => todo!(),
                // pulldown_cmark::TagEnd::Link => todo!(),
                // pulldown_cmark::TagEnd::Image => todo!(),
                // pulldown_cmark::TagEnd::MetadataBlock(metadata_block_kind) => todo!(),
            }
        }

        other => other, // Event::Text(cow_str) => todo!(),
                        // Event::Code(cow_str) => todo!(),
                        // Event::InlineMath(cow_str) => todo!(),
                        // Event::DisplayMath(cow_str) => todo!(),
                        // Event::Html(cow_str) => todo!(),
                        // Event::InlineHtml(cow_str) => todo!(),
                        // Event::FootnoteReference(cow_str) => todo!(),
                        // Event::SoftBreak => todo!(),
                        // Event::HardBreak => todo!(),
                        // Event::Rule => todo!(),
                        // Event::TaskListMarker(_) => todo!(),
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
