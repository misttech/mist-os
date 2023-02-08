// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{trace_duration, Benchmark, Filesystem, OperationDuration, OperationTimer},
    async_trait::async_trait,
    std::{
        collections::VecDeque,
        ffi::{CStr, CString},
        mem::MaybeUninit,
        os::{fd::RawFd, unix::ffi::OsStringExt},
        path::PathBuf,
        vec::Vec,
    },
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
                let path = dir.path.join(file_name(i));
                std::fs::File::create(path).unwrap();
            }
            if self.max_depth == dir.depth {
                continue;
            }
            for i in 0..self.directories_per_directory {
                let path = dir.path.join(dir_name(i));
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

/// A benchmark that measures how long it takes to call stat on a path to a file. A distinct file is
/// used for each iteration.
#[derive(Clone)]
pub struct StatPath {
    file_count: u64,
}

impl StatPath {
    pub fn new() -> Self {
        Self { file_count: 100 }
    }
}

#[async_trait]
impl Benchmark for StatPath {
    async fn run(&self, fs: &mut dyn Filesystem) -> Vec<OperationDuration> {
        trace_duration!("benchmark", "StatPath");

        let root = fs.benchmark_dir().to_path_buf();
        for i in 0..self.file_count {
            std::fs::File::create(root.join(file_name(i))).unwrap();
        }

        let root_fd =
            open_path(&path_buf_to_c_string(root), libc::O_DIRECTORY | libc::O_RDONLY).unwrap();

        let mut durations = Vec::with_capacity(self.file_count as usize);
        for i in 0..self.file_count {
            let path = path_buf_to_c_string(file_name(i));
            trace_duration!("benchmark", "stat", "file" => i);
            let timer = OperationTimer::start();
            stat_path_at(&root_fd, &path).unwrap();
            durations.push(timer.stop());
        }
        durations
    }

    fn name(&self) -> String {
        "StatPath".to_string()
    }
}

/// A benchmark that measures how long it takes to open a file. A distinct file is used for each
/// iteration.
#[derive(Clone)]
pub struct OpenFile {
    file_count: u64,
}

impl OpenFile {
    pub fn new() -> Self {
        Self { file_count: 100 }
    }
}

#[async_trait]
impl Benchmark for OpenFile {
    async fn run(&self, fs: &mut dyn Filesystem) -> Vec<OperationDuration> {
        trace_duration!("benchmark", "OpenFile");

        let root = fs.benchmark_dir().to_path_buf();
        for i in 0..self.file_count {
            std::fs::File::create(root.join(file_name(i))).unwrap();
        }

        let root_fd =
            open_path(&path_buf_to_c_string(root), libc::O_DIRECTORY | libc::O_RDONLY).unwrap();

        let mut durations = Vec::with_capacity(self.file_count as usize);
        for i in 0..self.file_count {
            let path = path_buf_to_c_string(file_name(i));
            // Pull the file outside of the trace so it doesn't capture the close call.
            let _file = {
                trace_duration!("benchmark", "open", "file" => i);
                let timer = OperationTimer::start();
                let file = open_path_at(&root_fd, &path, libc::O_RDWR);
                durations.push(timer.stop());
                file
            };
        }
        durations
    }

    fn name(&self) -> String {
        "OpenFile".to_string()
    }
}

/// A benchmark that measures how long it takes to open a file from a path that contains multiple
/// directories. A distinct path and file is used for each iteration.
#[derive(Clone)]
pub struct OpenDeeplyNestedFile {
    file_count: u64,
    depth: u64,
}

impl OpenDeeplyNestedFile {
    pub fn new() -> Self {
        Self { file_count: 100, depth: 5 }
    }

    fn dir_path(&self, n: u64) -> PathBuf {
        let mut path = PathBuf::new();
        for i in 0..self.depth {
            path = path.join(format!("dir-{:03}-{:03}", n, i));
        }
        path
    }
}

#[async_trait]
impl Benchmark for OpenDeeplyNestedFile {
    async fn run(&self, fs: &mut dyn Filesystem) -> Vec<OperationDuration> {
        const FILE_COUNT: usize = 100;
        trace_duration!("benchmark", "OpenDeeplyNestedFile");

        let root = fs.benchmark_dir().to_path_buf();
        for i in 0..self.file_count {
            let dir_path = root.join(self.dir_path(i));
            std::fs::create_dir_all(&dir_path).unwrap();
            std::fs::File::create(dir_path.join(file_name(i))).unwrap();
        }

        let root_fd =
            open_path(&path_buf_to_c_string(root), libc::O_DIRECTORY | libc::O_RDONLY).unwrap();

        let mut durations = Vec::with_capacity(self.file_count as usize);
        for i in 0..self.file_count {
            let path = path_buf_to_c_string(self.dir_path(i).join(file_name(i)));
            // Pull the file outside of the trace so it doesn't capture the close call.
            let _file = {
                trace_duration!("benchmark", "open", "file" => i);
                let timer = OperationTimer::start();
                let file = open_path_at(&root_fd, &path, libc::O_RDWR);
                durations.push(timer.stop());
                file
            };
        }
        durations
    }

    fn name(&self) -> String {
        "OpenDeeplyNestedFile".to_string()
    }
}

pub fn file_name(n: u64) -> PathBuf {
    format!("file-{:03}.txt", n).into()
}

pub fn dir_name(n: u64) -> PathBuf {
    format!("dir-{:03}", n).into()
}

pub fn path_buf_to_c_string(path: PathBuf) -> CString {
    CString::new(path.into_os_string().into_vec()).unwrap()
}

pub fn stat_path_at(dir: &OpenFd, path: &CStr) -> Result<libc::stat, std::io::Error> {
    unsafe {
        let mut stat = MaybeUninit::uninit();
        // `libc::fstatat` is directly used instead of `std::fs::metadata` because
        // `std::fs::metadata` uses `statx` on Linux.
        let result =
            libc::fstatat(dir.0, path.as_ptr(), stat.as_mut_ptr(), libc::AT_SYMLINK_NOFOLLOW);

        if result == 0 {
            Ok(stat.assume_init())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
}

pub struct OpenFd(RawFd);

impl Drop for OpenFd {
    fn drop(&mut self) {
        unsafe { libc::close(self.0) };
    }
}

pub fn open_path(path: &CStr, flags: libc::c_int) -> Result<OpenFd, std::io::Error> {
    let result = unsafe { libc::open(path.as_ptr(), flags) };
    if result >= 0 {
        Ok(OpenFd(result))
    } else {
        Err(std::io::Error::last_os_error())
    }
}

pub fn open_path_at(
    dir: &OpenFd,
    path: &CStr,
    flags: libc::c_int,
) -> Result<OpenFd, std::io::Error> {
    let result = unsafe { libc::openat(dir.0, path.as_ptr(), flags) };
    if result >= 0 {
        Ok(OpenFd(result))
    } else {
        Err(std::io::Error::last_os_error())
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

    #[fuchsia::test]
    async fn stat_path_test() {
        let mut test_fs = Box::new(TestFilesystem::new());
        let benchmark = StatPath { file_count: 5 };
        let results = benchmark.run(test_fs.as_mut()).await;
        assert_eq!(results.len(), 5);
        assert_eq!(test_fs.clear_cache_count().await, 0);
        test_fs.shutdown().await;
    }

    #[fuchsia::test]
    async fn open_file_test() {
        let mut test_fs = Box::new(TestFilesystem::new());
        let benchmark = OpenFile { file_count: 5 };
        let results = benchmark.run(test_fs.as_mut()).await;
        assert_eq!(results.len(), 5);
        assert_eq!(test_fs.clear_cache_count().await, 0);
        test_fs.shutdown().await;
    }

    #[fuchsia::test]
    async fn open_deeply_nested_file_test() {
        let mut test_fs = Box::new(TestFilesystem::new());
        let benchmark = OpenDeeplyNestedFile { file_count: 5, depth: 3 };
        let results = benchmark.run(test_fs.as_mut()).await;
        assert_eq!(results.len(), 5);
        assert_eq!(test_fs.clear_cache_count().await, 0);
        test_fs.shutdown().await;
    }
}
