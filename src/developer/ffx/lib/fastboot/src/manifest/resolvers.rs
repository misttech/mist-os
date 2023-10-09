// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    file_resolver::resolvers::{ArchiveResolver, Resolver, TarResolver},
    file_resolver::FileResolver,
};
use anyhow::Result;
use async_trait::async_trait;
use errors::ffx_bail;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

/// Trait that allows the given Resolvers to find out where their flash manifest paths are
#[async_trait]
pub trait ManifestResolver {
    async fn get_manifest_path(&self) -> &Path;
}

#[async_trait]
impl ManifestResolver for TarResolver {
    async fn get_manifest_path(&self) -> &Path {
        self.manifest()
    }
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
