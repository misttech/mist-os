// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ffx_config::{EnvironmentContext, Sdk};
use std::path::{Path, PathBuf};

/// Wraps the symbolizer host tool and ensures it has the dependencies it needs to run.
#[derive(Debug)]
pub struct Symbolizer {
    sdk: Sdk,
}

impl Symbolizer {
    /// Create a new symbolizer runner.
    pub async fn new() -> Result<Self, CreateSymbolizerError> {
        let context = ffx_config::global_env_context()
            .ok_or(CreateSymbolizerError::NoFfxEnvironmentContext)?;
        Self::with_context(&context).await
    }

    /// Create a new symbolizer with a specific ffx context. Normally only needed in tests.
    pub async fn with_context(context: &EnvironmentContext) -> Result<Self, CreateSymbolizerError> {
        let sdk = context.get_sdk().await.map_err(CreateSymbolizerError::NoSdkAvailable)?;

        symbol_index::ensure_symbol_index_registered(&sdk)
            .await
            .map_err(CreateSymbolizerError::SymbolIndexRegistration)?;

        Ok(Self { sdk })
    }

    /// Return a symbolizer command with all arguments and necessary environment variables provided.
    /// stdio can be overridden before spawning.
    pub fn command(
        &self,
        options: SymbolizerOptions,
    ) -> Result<std::process::Command, CreateSymbolizerError> {
        let mut cmd = self
            .sdk
            .get_host_tool_command("symbolizer")
            .map_err(CreateSymbolizerError::NoSymbolizerHostTool)?;
        options.apply(&mut cmd);
        Ok(cmd)
    }
}

/// Errors that can occur when creating the symbolizer.
#[derive(Debug, thiserror::Error)]
pub enum CreateSymbolizerError {
    /// No global environment context available.
    #[error("ffx couldn't find a global environment context.")]
    NoFfxEnvironmentContext,

    /// Couldn't retrieve an SDK for ffx.
    #[error("ffx couldn't find an SDK to use.")]
    NoSdkAvailable(#[source] anyhow::Error),

    /// Couldn't register a symbol index.
    #[error("ffx couldn't register the symbol index.")]
    SymbolIndexRegistration(#[source] anyhow::Error),

    /// Failed to get a symbolizer host tool.
    #[error("ffx couldn't find a symbolizer binary in the SDK.")]
    NoSymbolizerHostTool(#[source] anyhow::Error),
}

/// Options for running the symbolizer.
#[derive(Debug, Default)]
pub struct SymbolizerOptions {
    build_id_dir: Vec<PathBuf>,
    dumpfile_output: Option<PathBuf>,
    ids_txt: Vec<PathBuf>,
    omit_module_lines: bool,
    prettify_backtrace: bool,
    symbol_cache: Option<PathBuf>,
    symbol_index: Option<PathBuf>,
    symbol_path: Vec<PathBuf>,
    symbol_server: Option<String>,
}

impl SymbolizerOptions {
    fn apply(self, cmd: &mut std::process::Command) {
        for dir in self.build_id_dir {
            cmd.arg(format!("--build-id-dir={}", dir.display()));
        }
        if let Some(output) = self.dumpfile_output {
            cmd.arg(format!("--dumpfile-output={}", output.display()));
        }
        for file in self.ids_txt {
            cmd.arg(format!("--ids-txt={}", file.display()));
        }
        if self.omit_module_lines {
            cmd.arg("--omit-module-lines");
        }
        if self.prettify_backtrace {
            cmd.arg("--prettify-backtrace");
        }
        if let Some(cache) = self.symbol_cache {
            cmd.arg(format!("--symbol-cache={}", cache.display()));
        }
        if let Some(index) = self.symbol_index {
            cmd.arg(format!("--symbol-index={}", index.display()));
        }
        for path in self.symbol_path {
            cmd.arg(format!("--symbol-path={}", path.display()));
        }
        if let Some(url) = self.symbol_server {
            cmd.arg(format!("--symbol-server={}", url));
        }
    }

    ///  Adds the given directory to the symbol search path. Multiple
    ///  paths can be passed to add multiple directories.
    ///  The directory must have the same structure as a .build-id directory,
    ///  that is, each symbol file lives at xx/yyyyyyyy.debug where xx is
    ///  the first two characters of the build ID and yyyyyyyy is the rest.
    ///  However, the name of the directory doesn't need to be .build-id.
    pub fn build_id_dir(mut self, p: impl AsRef<Path>) -> Self {
        self.build_id_dir.push(p.as_ref().to_owned());
        self
    }

    /// Write the dumpfile output to the given file.
    pub fn dumpfile_output(mut self, p: impl AsRef<Path>) -> Self {
        self.dumpfile_output = Some(p.as_ref().to_owned());
        self
    }

    /// Adds the given file to the symbol search path. Multiple paths
    /// can be passed to add multiple files. The file, typically named
    /// "ids.txt", serves as a mapping from build ID to symbol file path and
    /// should contain multiple lines in the format of "<build ID> <file path>".
    pub fn ids_txt(mut self, p: impl AsRef<Path>) -> Self {
        self.ids_txt.push(p.as_ref().to_owned());
        self
    }

    /// Omit the "[[[ELF module ...]]]" lines from the output.
    pub fn omit_module_lines(mut self, omit: bool) -> Self {
        self.omit_module_lines = omit;
        self
    }

    /// Try to prettify backtraces.
    pub fn prettify_backtrace(mut self, prettify: bool) -> Self {
        self.prettify_backtrace = prettify;
        self
    }

    ///  Directory where we can keep a symbol cache, which defaults to
    ///  ~/.fuchsia/debug/symbol-cache. If a symbol server has been specified,
    ///  downloaded symbols will be stored in this directory. The directory
    ///  structure will be the same as a .build-id directory, and symbols will
    ///  be read from this location as though you had specified
    ///  "--build-id-dir=<path>".
    pub fn symbol_cache(mut self, cache: impl AsRef<Path>) -> Self {
        self.symbol_cache = Some(cache.as_ref().to_owned());
        self
    }

    /// Populates ids_txt and build_id_dir using the given symbol-index file,
    /// which defaults to ~/.fuchsia/debug/symbol-index. The file should be
    /// created and maintained by the "symbol-index" host tool.
    pub fn symbol_index(mut self, index: impl AsRef<Path>) -> Self {
        self.symbol_index = Some(index.as_ref().to_owned());
        self
    }

    /// Adds the given directory or file to the symbol search path. Multiple
    /// paths can be passed to add multiple locations. When a directory
    /// path is passed, the directory will be enumerated non-recursively to
    /// index all ELF files. When a file is passed, it will be loaded as an ELF
    /// file (if possible).
    pub fn symbol_path(mut self, path: impl AsRef<Path>) -> Self {
        self.symbol_path.push(path.as_ref().to_owned());
        self
    }

    /// Adds the given URL to symbol servers. Symbol servers host the debug
    /// symbols for prebuilt binaries and dynamic libraries.
    pub fn symbol_server(mut self, url: impl AsRef<str>) -> Self {
        self.symbol_server = Some(url.as_ref().to_owned());
        self
    }
}
