// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::BlockDeviceFactory,
    async_trait::async_trait,
    std::path::{Path, PathBuf},
};

/// A trait for creating `Filesystem`s.
#[async_trait]
pub trait FilesystemConfig: Send + Sync {
    /// The concrete type of the filesystem started by `start_filesystem`. It must at least
    /// implement the `Filesystem` trait.
    type Filesystem: Filesystem;

    /// Starts an instance of the filesystem.
    async fn start_filesystem(
        &self,
        block_device_factory: &dyn BlockDeviceFactory,
    ) -> Self::Filesystem;

    /// The name of the filesystem. This is used for filtering benchmarks and outputting results.
    fn name(&self) -> String;
}

/// A trait representing a mounted filesystem that benchmarks will be run against.
#[async_trait]
pub trait Filesystem: Send + Sync {
    /// Cleans up the filesystem after a benchmark has run.
    async fn shutdown(self);

    /// Path to where the filesystem is located in the current process's namespace. All benchmark
    /// operations should happen within this directory.
    fn benchmark_dir(&self) -> &Path;
}

/// A trait for filesystems that are able to clear their caches.
#[async_trait]
pub trait CacheClearableFilesystem: Filesystem {
    /// Clears all cached data in the filesystem. This method is used in "cold" benchmarks to
    /// ensure that the filesystem isn't using cached data from the setup phase in the benchmark
    /// phase.
    async fn clear_cache(&mut self);
}

/// A `FilesystemConfig` for a filesystem that is already present in the process's namespace.
#[derive(Clone)]
pub struct MountedFilesystem {
    /// Path to an existing filesystem.
    dir: PathBuf,

    /// Name of the filesystem that backs `dir`.
    name: String,
}

impl MountedFilesystem {
    pub fn new<P: Into<PathBuf>>(dir: P, name: String) -> Self {
        Self { dir: dir.into(), name }
    }
}

#[async_trait]
impl FilesystemConfig for MountedFilesystem {
    type Filesystem = MountedFilesystemInstance;
    async fn start_filesystem(
        &self,
        _block_device_factory: &dyn BlockDeviceFactory,
    ) -> MountedFilesystemInstance {
        // Create a new directory within the existing filesystem to make it easier for cleaning up
        // afterwards.
        let path = self.dir.join("benchmark");
        std::fs::create_dir(&path).unwrap_or_else(|e| {
            panic!("failed to created benchmark directory '{}': {:?}", path.display(), e)
        });
        MountedFilesystemInstance::new(path)
    }

    fn name(&self) -> String {
        self.name.clone()
    }
}

/// A `Filesystem` instance for a filesystem that is already present in the process's namespace.
pub struct MountedFilesystemInstance {
    dir: PathBuf,
}

impl MountedFilesystemInstance {
    pub fn new<P: Into<PathBuf>>(dir: P) -> Self {
        Self { dir: dir.into() }
    }
}

#[async_trait]
impl Filesystem for MountedFilesystemInstance {
    async fn shutdown(self) {
        std::fs::remove_dir_all(self.dir).expect("Failed to remove benchmark directory");
    }

    fn benchmark_dir(&self) -> &Path {
        self.dir.as_path()
    }
}
