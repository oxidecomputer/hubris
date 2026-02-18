// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! This command prepares a directory containing every file we want to upload
//! into GitHub Actions artifacts in CI. Without attestations that's trivial (we
//! just upload all hubris archives for the images we built), but it gets more
//! complex when attestations enter the picture.
//!
//! With attestations, for each hubris archive we want to have a separate
//! `$archive.sigstore.json` file, containing the Sigstore Bundle v0.3 encoding
//! of the attestation for that archive. So, if we were for example building
//! oxide-rot-1, we'd have four files to upload as artifacts:
//!
//! - build-oxide-rot-1-image-a.zip
//! - build-oxide-rot-1-image-a.zip.sigstore.json
//! - build-oxide-rot-1-image-b.zip
//! - build-oxide-rot-1-image-b.zip.sigstore.json
//!
//! Unfortunately GitHub Actions's sigstore attestation action generates a
//! single file containing all of the attestations, one per line. We thus need
//! to figure out which attestation belongs to which archive and extract them
//! out of the merged file. That's the main complexity of this task.

use std::collections::HashMap;
use std::fs::File;
use std::path::Path;

use anyhow::{bail, Context as _, Result};
use base64::prelude::*;
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

use crate::dist::PackageConfig;

/// Copy all artifacts to upload for a given app.toml and attestation into a
/// temporary directory, and print the path of that directory to stdout.
pub fn run(cfg: &Path, attestations: Option<&Path>) -> Result<()> {
    // Create a clean directory that will contain the artifacts to upload.
    let dest = Path::new("target").join("gha-prepared-artifacts");
    if dest.exists() {
        std::fs::remove_dir_all(&dest)
            .with_context(|| format!("failed to remove {}", dest.display()))?;
    }
    std::fs::create_dir_all(&dest).with_context(|| {
        format!("failed to create directory {}", dest.display())
    })?;

    let config = PackageConfig::new(cfg, false, false)
        .context("could not create build configuration")?;

    let mut hashes = HashMap::new();
    for image_name in &config.toml.image_names {
        let archive_name = config.toml.archive_name(&image_name);
        let path = config.img_file(archive_name, &image_name);
        let file_name = path.file_name().expect("missing file name");
        let file_dest = dest.join(file_name);

        std::fs::copy(&path, &file_dest).with_context(|| {
            format!(
                "failed to copy {} to {}",
                path.display(),
                file_dest.display(),
            )
        })?;

        // The hash will be used later to determine which attestation refers to
        // this artifact (as the attestation contains the hash within it).
        let mut hasher = Sha256::new();
        std::io::copy(
            &mut File::open(&path).with_context(|| {
                format!("failed to open {}", dest.display())
            })?,
            &mut hasher,
        )
        .with_context(|| format!("failed to hash {}", dest.display()))?;
        hashes.insert(hasher.finalize().to_vec(), file_name.to_os_string());
    }

    if let Some(attestations) = attestations {
        let attestations =
            std::fs::read_to_string(attestations).with_context(|| {
                format!("failed to read {}", attestations.display())
            })?;

        for line in attestations.split('\n').map(str::trim) {
            if line.is_empty() {
                continue;
            }
            for hash in hashes_from_sigstore_bundle(line)? {
                if let Some(file_name) = hashes.remove(&hash) {
                    let mut sigstore_name = file_name.clone();
                    sigstore_name.push(".sigstore.json");
                    let sigstore_path = dest.join(sigstore_name);
                    std::fs::write(&sigstore_path, line.as_bytes())
                        .with_context(|| {
                            format!(
                                "failed to write to {}",
                                sigstore_path.display()
                            )
                        })?;
                }
            }
        }
        if !hashes.is_empty() {
            bail!(
                "some archives were not attested: {:?}",
                hashes.values().collect::<Vec<_>>()
            );
        }
    }

    println!("{}", dest.display());

    Ok(())
}

fn hashes_from_sigstore_bundle(raw: &str) -> Result<Vec<Vec<u8>>> {
    let b: Bundle = serde_json::from_str(raw).context("can't parse bundle")?;
    if b.media_type != "application/vnd.dev.sigstore.bundle+json;version=0.3"
        && b.media_type != "application/vnd.dev.sigstore.bundle.v0.3+json"
    {
        bail!("unsupported sigstore media type: {}", b.media_type);
    }

    match b.content {
        BundleContent::MessageSignature(ms) => {
            if ms.message_digest.algorithm != "SHA2_256" {
                bail!("only sha256 message digests are supported");
            }
            Ok(vec![BASE64_STANDARD
                .decode(ms.message_digest.digest)
                .context("message digest is not base64")?
                .to_vec()])
        }
        BundleContent::DsseEnvelope(dsse) => {
            if dsse.payload_type != "application/vnd.in-toto+json" {
                bail!("unsupported dsse payload type: {}", dsse.payload_type);
            }
            let intoto: InTotoStatement = serde_json::from_slice(
                &BASE64_STANDARD
                    .decode(dsse.payload)
                    .context("dsse payload is not base64")?,
            )
            .context("failed to parse dsse payload")?;
            if intoto.type_ != "https://in-toto.io/Statement/v1" {
                bail!("unsupported in-toto type: {}", intoto.type_);
            }

            let mut hashes = Vec::new();
            for subject in &intoto.subject {
                hashes.push(
                    hex::decode(&subject.digest.sha256)
                        .context("in-toto subject digest is not hex")?,
                );
            }
            Ok(hashes)
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Bundle {
    media_type: String,
    #[serde(flatten)]
    content: BundleContent,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
enum BundleContent {
    MessageSignature(BundleMessageSignature),
    DsseEnvelope(DsseEnvelope),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BundleMessageSignature {
    message_digest: BundleMessageDigest,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BundleMessageDigest {
    algorithm: String,
    digest: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DsseEnvelope {
    payload: String,
    payload_type: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InTotoStatement {
    #[serde(rename = "_type")]
    type_: String,
    subject: Vec<InTotoSubject>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InTotoSubject {
    pub digest: InTotoDigest,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InTotoDigest {
    pub sha256: String,
}
