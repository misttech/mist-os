// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{trace_duration, Benchmark, Filesystem, OperationDuration, OperationTimer},
    async_trait::async_trait,
    std::{collections::VecDeque, path::PathBuf, vec::Vec},
};

/// Describes the structure of a directory tree to be benchmarked.
#[derive(Copy, Clone)]
pub struct DirectoryTreeStructure {
    /// The number of files that will be created within each directory.
    pub files_per_directory: u64,

    /// The number of subdirectories that will be created within each directory. No subdirectories
    /// are created once |max_depth| is reached.
    pub directories_per_directory: u64,

    /// Specifies how many levels of directories to create.
    pub max_depth: u32,
}

impl DirectoryTreeStructure {
    /// Returns the maximum size of queue required to do a breadth first traversal of a directory
    /// tree with this structure.
    fn max_pending(&self) -> u64 {
        self.directories_per_directory.pow(self.max_depth)
    }

    /// Creates a directory tree as described by this object starting from |root|.
    fn create_directory_tree(&self, root: PathBuf) {
        struct DirInfo {
            path: PathBuf,
            depth: u32,
        }
        let mut pending = VecDeque::new();
        pending.push_back(DirInfo { path: root, depth: 0 });
        while let Some(dir) = pending.pop_front() {
            for i in 0..self.files_per_directory {
                let path = dir.path.join(format!("file-{}", i));
                std::fs::File::create(path).unwrap();
            }
            if self.max_depth == dir.depth {
                continue;
            }
            for i in 0..self.directories_per_directory {
                let path = dir.path.join(format!("dir-{}", i));
                std::fs::create_dir(&path).unwrap();
                pending.push_back(DirInfo { path, depth: dir.depth + 1 });
            }
        }
    }
}

/// A benchmark that measures how long it takes to walk a directory that should not already be
/// cached in memory.
#[derive(Clone)]
pub struct WalkDirectoryTreeCold {
    dts: DirectoryTreeStructure,
    iterations: u64,
}

impl WalkDirectoryTreeCold {
    pub fn new(dts: DirectoryTreeStructure, iterations: u64) -> Self {
        Self { dts, iterations }
    }
}

#[async_trait]
impl Benchmark for WalkDirectoryTreeCold {
    async fn run(&self, fs: &mut dyn Filesystem) -> Vec<OperationDuration> {
        trace_duration!(
            "benchmark",
            "WalkDirectoryTreeCold",
            "files_per_directory" => self.dts.files_per_directory,
            "directories_per_directory" => self.dts.directories_per_directory,
            "max_depth" => self.dts.max_depth
        );
        let root = fs.benchmark_dir().to_path_buf();

        self.dts.create_directory_tree(root.clone());
        let max_pending = self.dts.max_pending() as usize;
        let mut durations = Vec::new();
        for i in 0..self.iterations {
            fs.clear_cache().await;
            trace_duration!("benchmark", "WalkDirectoryTree", "iteration" => i);
            let timer = OperationTimer::start();
            walk_directory_tree(root.clone(), max_pending);
            durations.push(timer.stop());
        }
        durations
    }

    fn name(&self) -> String {
        format!(
            "WalkDirectoryTreeCold/Files{}/Dirs{}/Depth{}",
            self.dts.files_per_directory, self.dts.directories_per_directory, self.dts.max_depth
        )
    }
}

/// A benchmark that measures how long it takes to walk a directory that was recently accessed and
/// may be cached in memory.
#[derive(Clone)]
pub struct WalkDirectoryTreeWarm {
    dts: DirectoryTreeStructure,
    iterations: u64,
}

impl WalkDirectoryTreeWarm {
    pub fn new(dts: DirectoryTreeStructure, iterations: u64) -> Self {
        Self { dts, iterations }
    }
}

#[async_trait]
impl Benchmark for WalkDirectoryTreeWarm {
    async fn run(&self, fs: &mut dyn Filesystem) -> Vec<OperationDuration> {
        trace_duration!(
            "benchmark",
            "WalkDirectoryTreeWarm",
            "files_per_directory" => self.dts.files_per_directory,
            "directories_per_directory" => self.dts.directories_per_directory,
            "max_depth" => self.dts.max_depth
        );
        let root = fs.benchmark_dir().to_path_buf();

        self.dts.create_directory_tree(root.clone());
        let max_pending = self.dts.max_pending() as usize;
        let mut durations = Vec::new();
        for i in 0..self.iterations {
            trace_duration!("benchmark", "WalkDirectoryTree", "iteration" => i);
            let timer = OperationTimer::start();
            walk_directory_tree(root.clone(), max_pending);
            durations.push(timer.stop());
        }
        durations
    }

    fn name(&self) -> String {
        format!(
            "WalkDirectoryTreeWarm/Files{}/Dirs{}/Depth{}",
            self.dts.files_per_directory, self.dts.directories_per_directory, self.dts.max_depth
        )
    }
}

/// Performs a breadth first traversal of the directory tree starting at |root|. As an optimization,
/// |max_pending| can be supplied to pre-allocate the queue needed for the breadth first traversal.
fn walk_directory_tree(root: PathBuf, max_pending: usize) {
    let mut pending = VecDeque::new();
    pending.reserve(max_pending);
    pending.push_back(root);
    while let Some(dir) = pending.pop_front() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                pending.push_back(entry.path());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use {super::*, crate::testing::TestFilesystem};

    const ITERATION_COUNT: u64 = 3;

    #[derive(PartialEq, Debug)]
    struct DirectoryTree {
        files: u64,
        directories: Vec<DirectoryTree>,
    }

    fn read_in_directory_tree(root: PathBuf) -> DirectoryTree {
        let mut tree = DirectoryTree { files: 0, directories: vec![] };
        for entry in std::fs::read_dir(root).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                tree.directories.push(read_in_directory_tree(entry.path()));
            } else {
                tree.files += 1;
            }
        }
        tree
    }

    #[fuchsia::test]
    async fn create_directory_tree_with_depth_of_zero_test() {
        let test_fs = Box::new(TestFilesystem::new());
        let dts = DirectoryTreeStructure {
            files_per_directory: 3,
            directories_per_directory: 2,
            max_depth: 0,
        };
        let root = test_fs.benchmark_dir().to_owned();
        dts.create_directory_tree(root.clone());

        let directory_tree = read_in_directory_tree(root);
        assert_eq!(directory_tree, DirectoryTree { files: 3, directories: vec![] });
        test_fs.shutdown().await;
    }

    #[fuchsia::test]
    async fn create_directory_tree_with_depth_of_two_test() {
        let test_fs = Box::new(TestFilesystem::new());
        let dts = DirectoryTreeStructure {
            files_per_directory: 4,
            directories_per_directory: 2,
            max_depth: 2,
        };
        let root = test_fs.benchmark_dir().to_owned();
        dts.create_directory_tree(root.clone());

        let directory_tree = read_in_directory_tree(root);
        assert_eq!(
            directory_tree,
            DirectoryTree {
                files: 4,
                directories: vec![
                    DirectoryTree {
                        files: 4,
                        directories: vec![
                            DirectoryTree { files: 4, directories: vec![] },
                            DirectoryTree { files: 4, directories: vec![] }
                        ]
                    },
                    DirectoryTree {
                        files: 4,
                        directories: vec![
                            DirectoryTree { files: 4, directories: vec![] },
                            DirectoryTree { files: 4, directories: vec![] }
                        ]
                    }
                ]
            }
        );
        test_fs.shutdown().await;
    }

    #[fuchsia::test]
    async fn walk_directory_tree_cold_test() {
        let mut test_fs = Box::new(TestFilesystem::new());
        let dts = DirectoryTreeStructure {
            files_per_directory: 2,
            directories_per_directory: 2,
            max_depth: 2,
        };
        let results = WalkDirectoryTreeCold::new(dts, ITERATION_COUNT).run(test_fs.as_mut()).await;

        assert_eq!(results.len(), ITERATION_COUNT as usize);
        assert_eq!(test_fs.clear_cache_count().await, ITERATION_COUNT);
        test_fs.shutdown().await;
    }

    #[fuchsia::test]
    async fn walk_directory_tree_warm_test() {
        let mut test_fs = Box::new(TestFilesystem::new());
        let dts = DirectoryTreeStructure {
            files_per_directory: 2,
            directories_per_directory: 2,
            max_depth: 2,
        };
        let results = WalkDirectoryTreeWarm::new(dts, ITERATION_COUNT).run(test_fs.as_mut()).await;

        assert_eq!(results.len(), ITERATION_COUNT as usize);
        assert_eq!(test_fs.clear_cache_count().await, 0);
        test_fs.shutdown().await;
    }
}
