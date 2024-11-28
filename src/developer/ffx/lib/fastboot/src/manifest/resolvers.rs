// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::common::done_time;
use crate::file_resolver::resolvers::{Resolver, TarResolver};
use crate::file_resolver::FileResolver;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::Utc;
use errors::{ffx_bail, ffx_error};
use std::fs::{create_dir_all, File};
use std::io::{copy, Write};
use std::path::{Path, PathBuf};
use tempfile::{tempdir, TempDir};
use walkdir::WalkDir;
use zip::ZipArchive;

/// Trait that allows the given Resolvers to find out where their flash manifest paths are
#[async_trait]
pub trait ManifestResolver {
    async fn get_manifest_path(&self) -> &Path;
}

#[async_trait]
impl ManifestResolver for ArchiveResolver {
    async fn get_manifest_path(&self) -> &Path {
        self.manifest()
    }
}

/// Specific resolver that embeds the base Resolver type. Ensures the root path is a file
pub struct FlashManifestResolver(Resolver);

impl FlashManifestResolver {
    pub fn new(path: PathBuf) -> Result<Self> {
        match path.is_file() {
            false => {
                ffx_bail!(
                    "Creating a FlashManifestResolver, path: {} is not a file",
                    path.display()
                );
            }
            true => Ok(Self(Resolver::new(path)?)),
        }
    }
}

#[async_trait(?Send)]
impl FileResolver for FlashManifestResolver {
    async fn get_file<W: Write>(&mut self, writer: &mut W, file: &str) -> Result<String> {
        self.0.get_file(writer, file).await
    }
}

#[async_trait]
impl ManifestResolver for FlashManifestResolver {
    async fn get_manifest_path(&self) -> &Path {
        self.0.root_path()
    }
}

pub struct ArchiveResolver {
    temp_dir: TempDir,
    manifest_path: PathBuf,
    internal_manifest_path: PathBuf,
    archive: ZipArchive<File>,
}

impl ArchiveResolver {
    pub fn new<W: Write>(writer: &mut W, path: PathBuf) -> Result<Self> {
        let temp_dir = tempdir()?;
        let file = File::open(path.clone())
            .map_err(|e| ffx_error!("Could not open archive file: {}", e))?;
        let mut archive =
            ZipArchive::new(file).map_err(|e| ffx_error!("Could not read archive: {}", e))?;
        let mut internal_manifest_path = None;
        let mut manifest_path = None;

        for i in 0..archive.len() {
            let mut archive_file = archive.by_index(i)?;
            let outpath = archive_file.sanitized_name();
            if (&*archive_file.name()).ends_with("flash.json")
                || (&*archive_file.name()).ends_with("flash-manifest.manifest")
            {
                let mut internal_path = PathBuf::new();
                internal_path.push(outpath.clone());
                internal_manifest_path.replace(internal_path.clone());
                let mut manifest = PathBuf::new();
                manifest.push(temp_dir.path());
                manifest.push(outpath);
                if let Some(p) = manifest.parent() {
                    if !p.exists() {
                        create_dir_all(&p)?;
                    }
                }
                let mut outfile = File::create(&manifest)?;
                let time = Utc::now();
                write!(
                    writer,
                    "Extracting {} to {}... ",
                    internal_path.file_name().expect("has a file name").to_string_lossy(),
                    temp_dir.path().display()
                )?;
                writer.flush()?;
                copy(&mut archive_file, &mut outfile)?;
                let duration = Utc::now().signed_duration_since(time);
                done_time(writer, duration)?;
                manifest_path.replace(manifest);
                break;
            }
        }

        match (internal_manifest_path, manifest_path) {
            (Some(i), Some(m)) => {
                Ok(Self { temp_dir, manifest_path: m, internal_manifest_path: i, archive })
            }
            _ => ffx_bail!("Could not locate flash manifest in archive: {}", path.display()),
        }
    }

    pub fn manifest(&self) -> &Path {
        self.manifest_path.as_path()
    }
}

#[async_trait(?Send)]
impl FileResolver for ArchiveResolver {
    async fn get_file<W: Write>(&mut self, writer: &mut W, file: &str) -> Result<String> {
        let mut file = match self.internal_manifest_path.parent() {
            Some(p) => {
                let mut path = PathBuf::new();
                path.push(p);
                path.push(file);
                self.archive
                    .by_name(path.to_str().ok_or_else(|| anyhow!("invalid archive file name"))?)
                    .map_err(|_| anyhow!("File not found in archive: {}", file))?
            }
            None => self
                .archive
                .by_name(file)
                .map_err(|_| anyhow!("File not found in archive: {}", file))?,
        };
        let mut outpath = PathBuf::new();
        outpath.push(self.temp_dir.path());
        outpath.push(file.sanitized_name());
        if let Some(p) = outpath.parent() {
            if !p.exists() {
                create_dir_all(&p)?;
            }
        }
        let time = Utc::now();
        write!(
            writer,
            "Extracting {} to {}... ",
            file.sanitized_name().file_name().expect("has a file name").to_string_lossy(),
            self.temp_dir.path().display()
        )?;
        if file.size() > (1 << 24) {
            write!(writer, "large file, please wait... ")?;
        }
        writer.flush()?;
        let mut outfile = File::create(&outpath)?;
        copy(&mut file, &mut outfile)?;
        let duration = Utc::now().signed_duration_since(time);
        done_time(writer, duration)?;
        Ok(outpath.to_str().ok_or_else(|| anyhow!("invalid temp file name"))?.to_owned())
    }
}

pub struct FlashManifestTarResolver(TarResolver, PathBuf);

impl FlashManifestTarResolver {
    pub fn new<W: Write>(writer: &mut W, path: PathBuf) -> Result<Self> {
        let resolver_inner = TarResolver::new(writer, path.clone())?;

        let manifest_path = WalkDir::new(resolver_inner.root_path())
            .into_iter()
            .filter_map(|e| e.ok())
            .find(|e| e.file_name() == "flash.json" || e.file_name() == "flash-manifest.manifest");

        match manifest_path {
            Some(m) => Ok(Self(resolver_inner, m.into_path())),
            _ => ffx_bail!("Could not locate flash manifest in archive: {}", path.display()),
        }
    }

    pub fn manifest(&self) -> &Path {
        self.1.as_path()
    }
}

#[async_trait(?Send)]
impl FileResolver for FlashManifestTarResolver {
    async fn get_file<W: Write>(&mut self, writer: &mut W, file: &str) -> Result<String> {
        self.0.get_file(writer, file).await
    }
}

#[async_trait]
impl ManifestResolver for FlashManifestTarResolver {
    async fn get_manifest_path(&self) -> &Path {
        self.1.as_path()
    }
}
