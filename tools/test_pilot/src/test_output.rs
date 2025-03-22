// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::path::PathBuf;

const TEST_CONFIG_FILE_PATH: &str = "test_config.json";
const TEST_STDOUT_FILE_PATH: &str = "host_binary_stdout.txt";
const TEST_STDERR_FILE_PATH: &str = "host_binary_stderr.txt";
const EXECUTION_LOG_FILE_PATH: &str = "execution_log.json";

/// Creates path values based on output directory structure.
pub struct OutputDirectory<'a> {
    path: &'a PathBuf,
}

impl<'a> OutputDirectory<'a> {
    pub fn new(path: &'a PathBuf) -> Self {
        Self { path }
    }

    pub fn test_config(&self) -> PathBuf {
        self.path.join(TEST_CONFIG_FILE_PATH)
    }

    pub fn test_stdout(&self) -> PathBuf {
        self.path.join(TEST_STDOUT_FILE_PATH)
    }

    pub fn test_stderr(&self) -> PathBuf {
        self.path.join(TEST_STDERR_FILE_PATH)
    }

    pub fn postprocessor_stdout(&self, binary: &PathBuf) -> PathBuf {
        self.path.join(format!(
            "{}_stdout.txt",
            binary
                .file_name()
                .expect("binary path has file name")
                .to_str()
                .expect("file name is ansi")
        ))
    }

    pub fn postprocessor_stderr(&self, binary: &PathBuf) -> PathBuf {
        self.path.join(format!(
            "{}_stderr.txt",
            binary
                .file_name()
                .expect("binary path has file name")
                .to_str()
                .expect("file name is ansi")
        ))
    }

    pub fn execution_log(&self) -> PathBuf {
        self.path.join(EXECUTION_LOG_FILE_PATH)
    }

    pub fn make_relative(&self, path: &PathBuf) -> Option<PathBuf> {
        path.strip_prefix(self.path).ok().map(|p| p.to_path_buf())
    }
}
