// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ffx_config_domain::ConfigDomain;
use std::{
    fmt,
    path::{Path, PathBuf},
};

/// The type of environment we're running in, along with relevant information about
/// that environment.
#[derive(Clone, Debug, PartialEq)]
pub enum EnvironmentKind {
    /// In a project with a fuchsia_env file at its root with config domain info
    /// in it.
    ConfigDomain { domain: ConfigDomain, isolate_root: Option<PathBuf> },
    /// In a fuchsia.git build tree with a jiri root and possibly a build directory.
    InTree { tree_root: PathBuf, build_dir: Option<PathBuf> },
    /// Isolated within a particular directory for testing or consistency purposes
    Isolated { isolate_root: PathBuf },
    /// Any other context with no specific information, using the user directory for
    /// all (non-global/default) configuration.
    NoContext,
}

impl EnvironmentKind {
    /// Get the isolate root of this environment
    pub fn isolate_root(&self) -> Option<&Path> {
        match self {
            Self::ConfigDomain { isolate_root, .. } => isolate_root.as_deref(),
            Self::Isolated { isolate_root } => Some(&isolate_root),
            _ => None,
        }
    }
}

impl std::fmt::Display for EnvironmentKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use EnvironmentKind::*;
        match self {
            ConfigDomain { domain, isolate_root: None } => {
                write!(f, "Fuchsia Project Rooted at {}", domain.root(),)
            }
            ConfigDomain { domain, isolate_root: Some(isolation_root) } => write!(
                f,
                "Fuchsia Project Rooted at {} using isolation root at {}",
                domain.root(),
                isolation_root.display(),
            ),
            InTree { tree_root, build_dir: Some(build_dir) } => write!(
                f,
                "Fuchsia.git In-Tree Rooted at {root}, with default build directory of {build}",
                root = tree_root.display(),
                build = build_dir.display()
            ),
            InTree { tree_root, build_dir: None } => write!(
                f,
                "Fuchsia.git In-Tree Root at {root} with no default build directory",
                root = tree_root.display()
            ),
            Isolated { isolate_root } => write!(
                f,
                "Isolated environment with an isolated root of {root}",
                root = isolate_root.display()
            ),
            NoContext => write!(f, "Global user context"),
        }
    }
}

/// What kind of executable target this environment was created in
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ExecutableKind {
    /// A main "launcher" ffx that can be used to run other ffx commands.
    MainFfx,
    /// A subtool ffx that only knows its own command.
    Subtool,
    /// In a unit or integration test
    Test,
}
