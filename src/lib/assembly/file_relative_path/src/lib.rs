// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![deny(missing_docs)]

//! Library that provides a struct and a trait which used to implement the
//! conversion of file-relative paths in config files to be relative to some
//! other base (such as the current working dir), and back.

mod file_relative_path;

pub use assembly_file_relative_path_derive::SupportsFileRelativePaths;
pub use file_relative_path::{FileRelativePathBuf, SupportsFileRelativePaths};
