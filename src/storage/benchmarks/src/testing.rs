// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{filesystem::MountedFilesystemInstance, Filesystem},
    async_trait::async_trait,
    fidl_fuchsia_io::DirectoryProxy,
    futures::lock::Mutex,
    std::{
        path::{Path, PathBuf},
        sync::Arc,
    },
    tempfile::TempDir,
};

/// Filesystem implementation that records `clear_cache` calls for validating warm and cold
/// benchmarks.
#[derive(Clone)]
pub struct TestFilesystem {
    benchmark_dir: PathBuf,
    inner: Arc<Mutex<TestFilesystemInner>>,
}

struct TestFilesystemInner {
    fs: Option<Box<MountedFilesystemInstance>>,
    clear_cache_count: u64,
}

impl TestFilesystem {
    pub fn new() -> Self {
        let benchmark_dir = TempDir::new_in("/tmp").unwrap().into_path();
        Self {
            inner: Arc::new(Mutex::new(TestFilesystemInner {
                fs: Some(Box::new(MountedFilesystemInstance::new(&benchmark_dir))),
                clear_cache_count: 0,
            })),
            benchmark_dir,
        }
    }

    pub async fn clear_cache_count(&self) -> u64 {
        self.inner.lock().await.clear_cache_count
    }
}

#[async_trait]
impl Filesystem for TestFilesystem {
    async fn clear_cache(&mut self) {
        let mut inner = self.inner.lock().await;
        inner.clear_cache_count += 1;
    }

    async fn shutdown(self: Box<Self>) {
        let mut inner = self.inner.lock().await;
        inner.fs.take().unwrap().shutdown().await;
    }

    fn benchmark_dir(&self) -> &Path {
        &self.benchmark_dir
    }

    fn exposed_dir(&mut self) -> &DirectoryProxy {
        unreachable!()
    }
}

impl Drop for TestFilesystem {
    fn drop(&mut self) {
        assert!(
            !self.benchmark_dir.try_exists().unwrap(),
            "The benchmark directory still exists. Shutdown may not have been called."
        );
    }
}
