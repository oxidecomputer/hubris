// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::dist::PackageConfig;
use anyhow::{Context, Result, anyhow, bail};
use std::collections::{BTreeMap, HashSet};
use std::io::Write;

pub struct HubrisTargetTask {
    pub manifest: std::path::PathBuf,
    pub task_name: String,
}

struct LspConfig {
    app_name: String,
    task_name: String,
    extra_env: BTreeMap<String, String>,
    target: String,
    features: Vec<String>,
    known_dirs: Vec<std::path::PathBuf>,
}

type JsonObject = serde_json::Map<String, serde_json::Value>;

impl LspConfig {
    /// Applies patches from the config to a JSON object
    ///
    /// The JSON object should be the `rust-analyzer` section (described
    /// [here](https://rust-analyzer.github.io/book/configuration.html)), which
    /// is `initializationOptions` in the `initialize` message sent by the
    /// editor.
    fn patch_options(&self, options: &mut JsonObject) {
        let cargo = get_or_insert(options, "cargo");
        cargo.insert("noDefaultFeatures".to_string(), true.into());
        if !cargo.contains_key("features") {
            cargo.insert(
                "features".to_string(),
                serde_json::Value::Array(Default::default()),
            );
        }
        let features = cargo
            .get_mut("features")
            .and_then(|f| f.as_array_mut())
            .expect("`features` must be an array");
        features.extend(self.features.iter().map(|v| v.clone().into()));
        let extra_env = get_or_insert(cargo, "extraEnv");
        extra_env.extend(
            self.extra_env
                .iter()
                .map(|(k, v)| (k.clone(), v.clone().into())),
        );
        cargo.insert("target".to_owned(), self.target.to_owned().into());

        // Only check the package being edited, to avoid errors from all the
        // other crates in the workspace (which may be incompatible with this
        // specific task's build configuration)
        let check = get_or_insert(options, "check");
        check.insert("workspace".to_owned(), false.into());
    }
}

pub fn run(
    log: Option<std::path::PathBuf>,
    target: Option<HubrisTargetTask>,
) -> Result<()> {
    let bonus_config = if let Some(HubrisTargetTask {
        manifest,
        task_name,
    }) = &target
    {
        let app_cfg = PackageConfig::new(manifest, false, false).context(
            format!("could not open manifest at {}", manifest.display()),
        )?;
        let task = app_cfg
            .toml
            .tasks
            .get(task_name)
            .ok_or_else(|| anyhow!("could not find task `{task_name}`"))?;
        let build_cfg = app_cfg
            .toml
            .task_build_config(task_name, false, None)
            .map_err(|_| {
                anyhow!("could not get build config for {task_name}")
            })?;

        // Find the `--target` argument
        let mut iter = build_cfg.args.iter();
        let mut target = None;
        while let Some(t) = iter.next() {
            if t == "--target" {
                iter.next().clone_into(&mut target);
            }
        }
        let Some(target) = target else {
            bail!("missing --target argument in build config");
        };

        // Use guppy to figure out what features should be enabled for
        // downstream crates; we'll add this to the build environment.
        let cmd = guppy::MetadataCommand::new();
        let package_graph = cmd.build_graph()?;
        let start_pkg = package_graph
            .packages()
            .find(|pkg| pkg.name() == task.name)
            .ok_or_else(|| {
                anyhow::anyhow!("crate `{}` not found in graph", task.name)
            })?;
        let feature_graph = package_graph.feature_graph();
        let feature_query =
            feature_graph.query_forward(task.features.iter().map(|feat| {
                guppy::graph::feature::FeatureId::new(
                    start_pkg.id(),
                    guppy::graph::feature::FeatureLabel::Named(feat),
                )
            }))?;
        let feature_set = feature_query.resolve();

        // Accumulate both features and manifest directories (so that we can
        // warn if a file is being edited outside our known set of packages)
        let mut features = vec![];
        let mut known_dirs = vec![];
        for feature_list in feature_set
            .packages_with_features(guppy::graph::DependencyDirection::Forward)
            .filter(|fl| fl.package().in_workspace())
        {
            let package = feature_list.package();
            let crate_name = package.name();
            features.extend(
                feature_list
                    .named_features()
                    .map(|f| format!("{crate_name}/{f}")),
            );
            // canonicalize to handle symlinks / `..` segments consistently
            let dir = package
                .manifest_path()
                .parent()
                .unwrap()
                .to_path_buf()
                .canonicalize()
                .unwrap();
            known_dirs.push(dir);
        }

        Some(LspConfig {
            app_name: app_cfg.toml.name,
            task_name: task_name.to_owned(),
            extra_env: build_cfg.env,
            target: target.clone(),
            features,
            known_dirs,
        })
    } else {
        None
    };

    let out = std::process::Command::new("rustup")
        .args(["show", "active-toolchain"])
        .output()
        .context("could not run `rustup`")?;
    if !out.status.success() {
        bail!(
            "rustup failed with exit code {}{}{}",
            out.status,
            if out.stdout.is_empty() {
                "".to_owned()
            } else {
                format!("\nstdout:\n{}", String::from_utf8_lossy(&out.stdout))
            },
            if out.stderr.is_empty() {
                "".to_owned()
            } else {
                format!("\nstderr:\n{}", String::from_utf8_lossy(&out.stderr))
            },
        );
    }
    let stdout = std::str::from_utf8(&out.stdout)
        .context("could not parse stdout as utf-8")?;
    let toolchain = stdout
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow!("could not get toolchain"))?;

    let mut ra = std::process::Command::new("rustup")
        .arg("run")
        .arg(toolchain)
        .arg("rust-analyzer")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .stdin(std::process::Stdio::piped())
        .spawn()?;

    let ra_stdout = ra.stdout.take().expect("stdout should be present");
    let ra_stdin = ra.stdin.take().expect("stdin should be present");

    let (tx, rx) = std::sync::mpsc::channel();

    // Thread to listen to the text editor's `stdout` (our `stdin`)
    let tx_ = tx.clone();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        while let Some(v) = read_json_object(&mut stdin) {
            if tx_.send(Msg::EditorToLsp(v)).is_err() {
                break;
            }
        }
    });

    // Thread to listen to `rust-analyzer`'s stdout
    let tx_ = tx.clone();
    std::thread::spawn(move || {
        let mut ra_stdout = std::io::BufReader::new(ra_stdout);
        while let Some(v) = read_json_object(&mut ra_stdout) {
            if tx_.send(Msg::LspToEditor(v)).is_err() {
                break;
            }
        }
    });

    let log = if let Some(log_path) = &log {
        Some(std::fs::File::create(log_path).with_context(|| {
            format!("failed to create log file at `{}`", log_path.display())
        })?)
    } else {
        None
    };
    let mut worker = Worker {
        rx,
        to_editor: std::io::stdout().lock(),
        to_lsp: ra_stdin,
        cfg: bonus_config,
        pending_cfg: HashSet::new(),
        checked_files: HashSet::new(),
        msg_id: 0,
        log,
    };
    worker.run();
    ra.kill().unwrap();

    Ok(())
}

enum Msg {
    EditorToLsp(JsonObject),
    LspToEditor(JsonObject),
}

struct Worker<'a> {
    /// Channel for messages
    rx: std::sync::mpsc::Receiver<Msg>,

    /// Channel to write to the text editor (which is our `stdout`)
    to_editor: std::io::StdoutLock<'a>,

    /// Channel to write to `rust-analyzer` (which is its `stdin`)
    to_lsp: std::process::ChildStdin,

    /// Object containing bonus configuration
    cfg: Option<LspConfig>,

    /// Pending `workspace/configuration` messages which should be hotpatched
    pending_cfg: HashSet<u64>,

    /// Files which have already been checked in `textDocument/didOpen`
    checked_files: HashSet<String>,

    /// File for logging
    log: Option<std::fs::File>,

    /// Id used for synthetic messages (prefixed with `xtask-0/` to distinguish)
    msg_id: u64,
}

impl Worker<'_> {
    fn run(&mut self) {
        while let Ok(msg) = self.rx.recv() {
            match msg {
                Msg::LspToEditor(v) => self.lsp_to_editor(v),
                Msg::EditorToLsp(v) => self.editor_to_lsp(v),
            }
        }
    }

    fn write_log(&mut self, header: &'static str, v: &JsonObject) {
        if let Some(log) = &mut self.log {
            writeln!(
                log,
                "{header}\n{}\n",
                serde_json::to_string_pretty(v).unwrap()
            )
            .unwrap();
        }
    }

    fn lsp_to_editor(&mut self, v: JsonObject) {
        self.write_log("lsp -> editor", &v);
        if self.cfg.is_some()
            && let Some(method) = v.get("method")
            && method.as_str() == Some("workspace/configuration")
            && let Some(id) = v.get("id").and_then(|v| v.as_u64())
        {
            self.pending_cfg.insert(id);
        }
        write_json(&mut self.to_editor, v);
    }

    fn editor_to_lsp(&mut self, mut v: JsonObject) {
        if let Some(cfg) = &self.cfg {
            // We patch two different messages to inject our configuration:
            // - `initialize` (editor -> lsp)
            // - `workspace/configuration` (lsp -> editor, we patch the response
            //   below)
            if let Some(method) = v.get("method")
                && method.as_str() == Some("initialize")
                && let Some(params) = v.get_mut("params")
                && let Some(params) = params.as_object_mut()
            {
                let options = get_or_insert(params, "initializationOptions");
                cfg.patch_options(options);
            }

            if let Some(id) = v.get("id").and_then(|v| v.as_u64())
                && let Some(result) = v.get_mut("result")
                && let Some(result) = result.as_array_mut()
                && result.len() == 1
                && let Some(result) = result[0].as_object_mut()
                && self.pending_cfg.remove(&id)
            {
                cfg.patch_options(result);
            }

            // When we first open a file that's not in our set of known LSP
            if let Some(method) = v.get("method")
                && method.as_str() == Some("textDocument/didOpen")
                && let Some(params) = v.get("params")
                && let Some(params) = params.as_object()
                && let Some(doc) = params.get("textDocument")
                && let Some(doc) = doc.as_object()
                && let Some(uri) = doc.get("uri")
                && let Some(uri) = uri.as_str()
                && self.checked_files.insert(uri.to_owned())
                && let Ok(url) = url::Url::parse(uri)
                && let Ok(path) = url.to_file_path()
                && let Ok(file) = path.canonicalize()
            {
                let is_prefix =
                    cfg.known_dirs.iter().any(|root| file.starts_with(root));
                if !is_prefix {
                    eprintln!("looking for file {}", file.display());
                    for c in &cfg.known_dirs {
                        eprintln!("{}", c.display());
                    }
                    let id = format!("xtask-0/{}", self.msg_id);
                    self.msg_id += 1;
                    let msg = format!(
                        "This file is not used by {}:{}; \
                         LSP support will be degraded",
                        cfg.app_name, cfg.task_name,
                    );
                    write_json(
                        &mut self.to_editor,
                        serde_json::json!(
                            {
                              "method": "window/showMessageRequest",
                              "id": id,
                              "params": {
                                "type": 2, // warning
                                "message": msg,
                                "actions": [
                                  { "title": "okay :(" },
                                ],
                              }
                            }
                        ),
                    );
                }
            }
        }
        self.write_log("editor -> lsp", &v);
        if let Some(id) = v.get("id").and_then(|v| v.as_str())
            && id.starts_with("xtask-")
        {
            // Right now, we don't do any custom handling of interposer messages
        } else {
            write_json(&mut self.to_lsp, v);
        }
    }
}

/// Reads an LSP-formatted message and returns the JSON object payload
fn read_json_object<B: std::io::BufRead>(buf: &mut B) -> Option<JsonObject> {
    let mut line = String::new();

    // Read the Content-Length line
    buf.read_line(&mut line).unwrap();
    if line.is_empty() {
        return None; // EOF
    }
    let n = line
        .strip_prefix("Content-Length: ")
        .expect("missing `Content-Length: `")
        .strip_suffix("\r\n")
        .expect("missing trailing `\r\n`");
    let size: usize = n.parse().unwrap();

    // Read the empty line
    line.clear();
    buf.read_line(&mut line).unwrap();

    let mut data = vec![0u8; size]; // for trailing "\r\n"
    buf.read_exact(&mut data).unwrap();

    let s = String::from_utf8(data).expect("could not parse body as utf-8");
    let v = serde_json::from_str(&s).expect("could not parse JSON");
    let serde_json::Value::Object(m) = v else {
        panic!("expected JSON object, got {v}");
    };
    Some(m)
}

/// Writes an LSP-formatted JSON object to a `Write` stream
fn write_json<B: std::io::Write, V: Into<serde_json::Value>>(
    buf: &mut B,
    value: V,
) {
    let out = value.into().to_string();
    buf.write_all(format!("Content-Length: {}\r\n", out.len()).as_bytes())
        .unwrap();
    buf.write_all(b"\r\n").unwrap();
    buf.write_all(out.as_bytes()).unwrap();
    buf.flush().unwrap();
}

/// Helper function to get or insert an Object into a JSON map
fn get_or_insert<'a>(
    obj: &'a mut serde_json::Map<String, serde_json::Value>,
    v: &'static str,
) -> &'a mut serde_json::Map<String, serde_json::Value> {
    if !obj.contains_key(v) {
        obj.insert(v.to_owned(), serde_json::Value::Object(Default::default()));
    }
    obj.get_mut(v)
        .and_then(|o| o.as_object_mut())
        .unwrap_or_else(|| panic!("{v} should be an object"))
}
