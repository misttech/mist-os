// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::file_resolver::FileResolver;
use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use errors::{ffx_bail, ffx_error};
use flate2::read::GzDecoder;
use std::fs::{create_dir_all, File};
use std::io::copy;
use std::path::{Path, PathBuf};
use tar::Archive;
use tempfile::{tempdir, TempDir};
use zip::read::ZipArchive;

pub struct EmptyResolver {
    fake: PathBuf,
}

impl EmptyResolver {
    pub fn new() -> Result<Self> {
        let mut fake = std::env::current_dir()?;
        fake.push("fake");
        Ok(Self { fake })
    }

    pub fn manifest(&self) -> &Path {
        self.fake.as_path() //should never get used
    }
}

#[async_trait(?Send)]
impl FileResolver for EmptyResolver {
    async fn get_file(&mut self, file: &str) -> Result<String> {
        if PathBuf::from(file).is_absolute() {
            Ok(file.to_string())
        } else {
            let mut parent = std::env::current_dir()?;
            parent.push(file);
            if let Some(f) = parent.to_str() {
                Ok(f.to_string())
            } else {
                ffx_bail!("Only UTF-8 strings are currently supported in the flash manifest")
            }
        }
    }
}

pub struct Resolver {
    root_path: PathBuf,
}

impl Resolver {
    pub fn new(path: PathBuf) -> Result<Self> {
        Ok(Self {
            root_path: path.canonicalize().with_context(|| {
                format!("Getting absolute path of flashing manifest at {:?}", path)
            })?,
        })
    }

    pub fn root_path(&self) -> &Path {
        self.root_path.as_path()
    }
}

#[async_trait(?Send)]
impl FileResolver for Resolver {
    async fn get_file(&mut self, file: &str) -> Result<String> {
        if PathBuf::from(file).is_absolute() {
            Ok(file.to_string())
        } else if let Some(p) = self.root_path().parent() {
            let mut parent = p.to_path_buf();
            parent.push(file);
            if let Some(f) = parent.to_str() {
                Ok(f.to_string())
            } else {
                ffx_bail!("Only UTF-8 strings are currently supported file paths")
            }
        } else {
            bail!("Could not get file to upload");
        }
    }
}

#[derive(Debug)]
pub struct ZipArchiveResolver {
    temp_dir: TempDir,
    archive: ZipArchive<File>,
}

impl ZipArchiveResolver {
    pub fn new(path: PathBuf) -> Result<Self> {
        let temp_dir = tempdir()?;
        let file = File::open(path.clone())
            .map_err(|e| ffx_error!("Could not open archive file at {}. {}", path.display(), e))?;
        let archive =
            ZipArchive::new(file).map_err(|e| ffx_error!("Could not read archive: {}", e))?;

        Ok(Self { temp_dir, archive })
    }
}

#[async_trait(?Send)]
impl FileResolver for ZipArchiveResolver {
    async fn get_file(&mut self, file: &str) -> Result<String> {
        let mut file = self
            .archive
            .by_name(file)
            .map_err(|_| anyhow!("File not found in archive: {}", file))?;

        let mut outpath = PathBuf::new();
        outpath.push(self.temp_dir.path());
        outpath.push(file.sanitized_name());
        if let Some(p) = outpath.parent() {
            if !p.exists() {
                create_dir_all(&p)?;
            }
        }
        tracing::debug!("Extracting to {}", self.temp_dir.path().display());
        let mut outfile = File::create(&outpath)?;
        copy(&mut file, &mut outfile)?;
        Ok(outpath.to_str().ok_or_else(|| anyhow!("invalid temp file name"))?.to_owned())
    }
}

pub struct TarResolver {
    temp_dir: TempDir,
}

impl TarResolver {
    pub fn new(path: PathBuf) -> Result<Self> {
        let temp_dir = tempdir()?;
        let file = File::open(path.clone())
            .map_err(|e| ffx_error!("Could not open archive file: {}", e))?;
        tracing::debug!("Extracting to {}", temp_dir.path().display());
        // Tarballs can't do per file extraction well like Zip, so just unpack it all.
        match path.extension() {
            Some(ext) if ext == "tar.gz" || ext == "tgz" => {
                let mut archive = Archive::new(GzDecoder::new(file));
                archive.unpack(temp_dir.path())?;
            }
            Some(ext) if ext == "tar" => {
                let mut archive = Archive::new(file);
                archive.unpack(temp_dir.path())?;
            }
            _ => ffx_bail!("Invalid tar archive"),
        }

        Ok(Self { temp_dir })
    }

    pub fn root_path(&self) -> &Path {
        self.temp_dir.path()
    }
}

#[async_trait(?Send)]
impl FileResolver for TarResolver {
    async fn get_file(&mut self, file: &str) -> Result<String> {
        let mut parent = self.root_path().to_path_buf();
        parent.push(file);
        if let Some(f) = parent.to_str() {
            Ok(f.to_string())
        } else {
            ffx_bail!("Only UTF-8 strings are currently supported.")
        }
    }
}

////////////////////////////////////////////////////////////////////////////////
// tests

#[cfg(test)]
mod test {
    // use tempfile::NamedTempFile;

    use super::*;
    use std::io::{Read, Write};
    use std::str::FromStr;
    use zip::write::{FileOptions, ZipWriter};
    use zip::CompressionMethod;

    ////////////////////////////////////////////////////////////////////////////////
    // ZipArchiveResolver

    #[test]
    fn zip_archive_resolver_new_errors() -> Result<()> {
        let non_existant_path = PathBuf::from_str("./not-exists.zip")?;
        assert!(ZipArchiveResolver::new(non_existant_path).is_err());
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn zip_archive_resolver_get_file() -> Result<()> {
        // Make a temporary zip file
        let tmpdir = tempdir()?;

        let mut pbuff = PathBuf::new();
        pbuff.push(tmpdir.path());
        pbuff.push("test_zip.zip");

        let file = File::create(pbuff.as_path())?;

        // We use a buffer here, though you'd normally use a `File`
        let mut zip = ZipWriter::new(file);
        let options = FileOptions::default().compression_method(CompressionMethod::Stored);

        zip.start_file("hello_world.txt", options)?;
        let _ = zip.write(b"Hello, World!")?;
        zip.start_file("foo/hello_world.txt", options)?;
        let _ = zip.write(b"Hello, nested World!")?;

        zip.flush()?;
        zip.finish()?;

        let mut resolver = ZipArchiveResolver::new(pbuff)?;

        // Test standard file
        {
            let file_path = resolver.get_file("hello_world.txt").await?;
            let mut hello_file = File::open(file_path)?;
            let mut hello_buf = vec![];
            hello_file.read_to_end(&mut hello_buf)?;
            assert_eq!(hello_buf, b"Hello, World!");
        }

        // Test nested file
        {
            let file_path = resolver.get_file("foo/hello_world.txt").await?;
            let mut hello_file = File::open(file_path)?;
            let mut hello_buf = vec![];
            hello_file.read_to_end(&mut hello_buf)?;
            assert_eq!(hello_buf, b"Hello, nested World!");
        }

        // Test standard file with leading slash
        {
            assert!(resolver.get_file("/hello_world.txt").await.is_err());
        }

        // Test non-existent file
        {
            assert!(resolver.get_file("this-shouldnt-exist.txt").await.is_err());
        }

        Ok(())
    }
}
