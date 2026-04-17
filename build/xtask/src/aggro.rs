use crate::config::Config;
use anyhow::Result;
use pulldown_cmark::{Event, HeadingLevel, Tag, TagEnd, html};
use std::{fs, io::Write as _, fmt::Write as _, path::Path};

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

        task_docs.push((name.to_string(), taskdocpath));
    }

    for (task, docpath) in task_docs.iter() {
        println!("  * {task}: {docpath:?}");
    }

    let mut html_buf = prelude(&format!("\"{}\" Aggregate Docs", cfg.name));

    // Start with the app readme
    if let Some(readme) = cfg.docfile.as_ref() {
        let app_readme = std::fs::read_to_string(readme)?;
        let parser = pulldown_cmark::Parser::new_ext(
            &app_readme,
            // Todo: not *everything*? Probably just something fully GitHub
            // Flavored Markdown compatible?
            pulldown_cmark::Options::all(),
        );
        let mut base = readme.to_owned();
        base.pop();
        let stream = parser.map(|evt| touchup(evt, &base));
        html::push_html(&mut html_buf, stream);
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

fn touchup<'a>(evt: Event<'a>, _base: &'a Path) -> Event<'a> {
    match evt {
        Event::Start(tag) => {
            match tag {
                Tag::Link { link_type, dest_url, title, id } => {
                    if !dest_url.starts_with("http") {
                        // TODO: rewrite relative to `https://github.com/oxidecomputer/hubris/blob/master/`?
                        // use _base to figure out relative paths, we might also need to
                        println!("WARN: We should be rewriting {dest_url}!");
                    }
                    Event::Start(Tag::Link { link_type, dest_url, title, id })
                },
                Tag::Image { link_type, dest_url, title, id } => {
                    if !dest_url.starts_with("http") {
                        // TODO: rewrite relative to `https://github.com/oxidecomputer/hubris/blob/master/`?
                        // use _base to figure out relative paths, we might also need to
                        println!("WARN: We should be rewriting {dest_url}!");
                    }
                    Event::Start(Tag::Image { link_type, dest_url, title, id })
                },
                // Bump down headings one notch, to allow for top level docs
                Tag::Heading { level, id, classes, attrs } => {
                    let level = match level {
                        HeadingLevel::H1 => HeadingLevel::H2,
                        HeadingLevel::H2 => HeadingLevel::H3,
                        HeadingLevel::H3 => HeadingLevel::H4,
                        HeadingLevel::H4 => HeadingLevel::H5,
                        HeadingLevel::H5 => HeadingLevel::H6,
                        HeadingLevel::H6 => HeadingLevel::H6,
                    };
                    Event::Start(Tag::Heading { level, id, classes, attrs })
                },

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
        },
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
                },
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
        },

        other => other
        // Event::Text(cow_str) => todo!(),
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

fn prelude(title: &str) -> String {
    let mut out = String::new();
    out.push_str(PRELUDE_PART_ONE);
    writeln!(&mut out, "  <title>{title}</title>").ok();
    out.push_str(PRELUDE_PART_TWO);
    out
}

const MARKDOWN_FOOTER: &str = r#"
  </article>
</body>
</html>
"#;
