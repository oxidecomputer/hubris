use anyhow::{Context, Result, anyhow, bail};

pub fn run(manifest: Option<&std::path::Path>) -> Result<()> {
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
    let mut ra_stdin = ra.stdin.take().expect("stdin should be present");

    // Start worker thread to read data in one direction (while the main thread
    // handles the other direction)
    std::thread::spawn(move || {
        let mut stdout = std::io::stdout().lock();
        let mut ra_stdout = std::io::BufReader::new(ra_stdout);
        while let Some(body) = read_json(&mut ra_stdout) {
            write_json(&mut stdout, body);
        }
    });

    let mut stdin = std::io::stdin().lock();
    while let Some(body) = read_json(&mut stdin) {
        write_json(&mut ra_stdin, body);
    }
    ra.kill().unwrap();

    Ok(())
}

fn read_json<B: std::io::BufRead>(buf: &mut B) -> Option<serde_json::Value> {
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
    serde_json::from_str(&s).expect("could not parse JSON")
}

fn write_json<B: std::io::Write>(buf: &mut B, value: serde_json::Value) {
    let out = value.to_string();
    buf.write_all(format!("Content-Length: {}\r\n", out.len()).as_bytes())
        .unwrap();
    buf.write_all(b"\r\n").unwrap();
    buf.write_all(out.as_bytes()).unwrap();
    buf.flush().unwrap();
}
