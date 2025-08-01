// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::artifact::{Artifact, CIPDPackage};

use anyhow::{bail, Context, Result};
use camino::Utf8PathBuf;
use std::process::{Command, Stdio};

/// Cache of assembly artifacts that may be downloaded from CIPD.
pub struct ArtifactCache {
    cache: Utf8PathBuf,
    ensured_artifacts: Utf8PathBuf,
}

impl ArtifactCache {
    /// Construct a new ArtifactCache.
    pub fn new() -> Self {
        let home = std::env::home_dir().unwrap();
        let home = Utf8PathBuf::from_path_buf(home).unwrap();
        let cache = home.join(".fuchsia").join("cipd");
        let ensured_artifacts = cache.join("artifacts");
        Self { cache, ensured_artifacts }
    }

    /// Delete all ensured artifacts.
    pub fn purge(&self) -> Result<()> {
        if self.ensured_artifacts.exists() {
            std::fs::remove_dir_all(&self.ensured_artifacts).context("Purging CIPD packages")?;
        }
        Ok(())
    }

    /// Resolve an artifact to a local path, downloading it if necessary.
    pub fn resolve(&self, artifact: &Artifact) -> Result<Utf8PathBuf> {
        match artifact {
            Artifact::Local(path) => Ok(path.clone()),
            Artifact::CIPD(package) => self.download(package),
            Artifact::MOS(_) => bail!("MOS identifiers are not supported"),
        }
    }

    /// Download an artifact from CIPD and return the local path.
    fn download(&self, package: &CIPDPackage) -> Result<Utf8PathBuf> {
        // Prepare the output directory.
        let artifact_path = self.ensured_artifacts.join(&package.path);
        let artifact_name = artifact_path
            .file_name()
            .with_context(|| {
                format!("Artifact path does not have a file name: {}", &artifact_path)
            })?
            .to_string();
        let artifact_dir = artifact_path
            .parent()
            .with_context(|| format!("Artifact path does not have a parent: {}", &artifact_path))?;
        std::fs::create_dir_all(&artifact_dir)?;

        // Write the ensure file.
        let ensure_contents = format!("{} {}", package.path, package.tag);
        let ensure_path = artifact_dir.join(format!("{}.ensure", &artifact_name));
        std::fs::write(&ensure_path, &ensure_contents)?;

        println!("Downloading: {}", package);

        // Download from CIPD.
        let child = Command::new("cipd")
            .arg("ensure")
            .arg("-ensure-file")
            .arg(&ensure_path)
            .arg("-root")
            .arg(&artifact_path)
            .arg("-cache-dir")
            .arg(&self.cache)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("Failed to execute cipd command")?;
        let child_output = child.wait_with_output().context("Waiting for cipd to finish")?;
        if !child_output.status.success() {
            let code = child_output
                .status
                .code()
                .map(|i| i.to_string())
                .unwrap_or_else(|| "unexpected termination".to_string());
            let stdout = String::from_utf8_lossy(&child_output.stdout);
            let stderr = String::from_utf8_lossy(&child_output.stderr);
            bail!("cipd failed with code: {}\nstdout:\n{}\nstderr:\n{}\n", code, stdout, stderr,);
        }
        return Ok(artifact_path);
    }
}
