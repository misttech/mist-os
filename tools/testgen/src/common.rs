// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Error,
    chrono::Datelike,
    handlebars::{handlebars_helper, Handlebars},
    std::path::{Path, PathBuf},
    tempfile::TempDir,
    tracing::info,
    walkdir::{DirEntry, WalkDir},
};

/// Creates a `TemplateFile` for a handlebars template.
///
/// $file is the path to the handlebars template file relative to
/// testgen's templates/ directory. A leading $dir path can be provided
/// to strip that prefix from the output filename. Arguments should not
/// contain leading or trailing slashes.
///
/// # Examples
///
/// ```
/// # Reads from 'template/realm_factory/src/main.rs.hbrs' and
/// # chooses the output file 'realm_factory/src/main.rs'
/// hbrs_template_file!("realm_factory/src/main.rs")
///
/// # Reads from 'template/realm_factory/src/main.rs.hbrs' and
/// # chooses the output file 'src/main.rs'
/// hbrs_template_file!("realm_factory", "src/main.rs")
/// ```
macro_rules! hbrs_template_file {
    ($file:expr) => {
        TemplateFile {
            filename: $file,
            contents: include_str!(concat!("../templates/", $file, ".hbrs")),
        }
    };
    ($dir:expr, $file:expr) => {
        TemplateFile {
            filename: $file,
            contents: include_str!(concat!("../templates/", $dir, "/", $file, ".hbrs")),
        }
    };
}
// Make this template available throughout the crate.
pub(crate) use hbrs_template_file;

/// A template file for code generation.
/// filename is the output filename where the template code will be generated.
/// contents is the template string.
/// Do not construct directly, use `hbrs_template_file` instead.
pub(crate) struct TemplateFile {
    pub filename: &'static str,
    pub contents: &'static str,
}

/// Returns the value of the template variable 'realm_factory_binary_name'.
pub(crate) fn var_realm_factory_binary_name(component_name: &str) -> String {
    let binary_name = format!("{}_realm_factory", component_name);
    sanitize_rust_binary_name(&binary_name)
}

/// Returns the value of the template variable 'test_binary_name'.
pub(crate) fn var_test_binary_name(component_name: &str) -> String {
    let binary_name = format!("{}_test", component_name);
    sanitize_rust_binary_name(&binary_name)
}

/// Returns the value of the template variable 'test_package_name'.
pub(crate) fn var_test_package_name(component_name: &str) -> String {
    format!("{}-test", component_name)
}

/// Returns the value of the template variable 'fidl_library_name'.
pub(crate) fn var_fidl_library_name(component_name: &str) -> String {
    let library_name = format!("test.{}", component_name);
    let library_name = library_name.replace("_", "");
    let library_name = library_name.replace("-", "");
    library_name
}

/// Returns the value of the template variable 'fidl_rust_crate_name'.
pub(crate) fn var_fidl_rust_crate_name(component_name: &str) -> String {
    let crate_name = var_fidl_library_name(component_name);
    let crate_name = format!("fidl_{}", crate_name);
    let crate_name = crate_name.replace(".", "_");
    let crate_name = crate_name.replace("-", "_");
    crate_name
}

/// Replaces characters in `binary_name` with the same replacements
/// the Fuchsia build uses for rust binary names.
pub(crate) fn sanitize_rust_binary_name(binary_name: &str) -> String {
    let binary_name = String::from(binary_name);
    let binary_name = binary_name.replace("-", "_");
    binary_name
}

/// Creates a directory at `path` if `path` it does not exist.
pub(crate) fn dir_create_if_absent<P: AsRef<Path>>(path: P) -> Result<(), Error> {
    let path = path.as_ref();
    if !path.exists() {
        info!("Creating directory {}", path.display());
        std::fs::create_dir_all(path)?;
    }
    Ok(())
}

/// Recursively copies the directory at `src` to `dst`.
pub(crate) fn dir_copy(src: impl AsRef<Path>, dst: impl AsRef<Path>) -> Result<(), Error> {
    let src = src.as_ref();
    let dst = dst.as_ref();
    let skip_root = true;
    for entry in walk_dir(src, skip_root) {
        let entry_src = entry.path();
        let entry_dst = dst.join(path_relative_to(src, entry_src));
        if entry.file_type().is_dir() {
            dir_copy(entry_src, entry_dst)?;
        } else {
            file_copy(entry_src, entry_dst)?;
        }
    }
    Ok(())
}

/// Copies the file at `src` to `dst`, creating any parent directories if they do not exist.
pub(crate) fn file_copy(src: impl AsRef<Path>, dst: impl AsRef<Path>) -> Result<(), Error> {
    let src = src.as_ref();
    let dst = dst.as_ref();
    dir_create_if_absent(dst.parent().unwrap())?;
    info!("Copying file {} to {}", src.display(), dst.display());
    std::fs::copy(src, dst)?;
    Ok(())
}

/// Writes `contents` to `path`, creating any parent directories if they do not exist.
pub(crate) fn file_write<P: AsRef<Path>>(path: P, contents: &str) -> Result<(), Error> {
    let path = path.as_ref();
    dir_create_if_absent(path.parent().unwrap())?;
    info!("Writing file {}", path.display());
    std::fs::write(path, contents)?;
    Ok(())
}

/// Returns the stem of the path `path`.
///
/// The stem is the non-extension portion of the basename.
///
/// Examples:
///
///   path_file_stem("/a/b/stem.c") => "stem"
///   path_file_stem("/a/b/stem.c.gz") => "stem.c"
///   path_file_stem("/a/b/") => "b"
///   path_file_stem("/a/b//") => "b"
///
/// # Panics
///
/// This panics if the given path has no stem, which can only occur if path is the empty string.
pub(crate) fn path_file_stem<P: AsRef<Path>>(path: P) -> String {
    path.as_ref().file_stem().unwrap().to_str().unwrap().to_string()
}

/// Returns a path that, when appended to `base` yields `path`.
pub(crate) fn path_relative_to(base: impl AsRef<Path>, path: impl AsRef<Path>) -> PathBuf {
    path.as_ref().strip_prefix(base).unwrap().to_owned()
}

// Walks the directory at `path` skipping '.' and '..'.
// If `skip_root` is true the returned iterator skips `path` and only iterates over its children.
pub(crate) fn walk_dir<P: AsRef<Path>>(path: P, skip_root: bool) -> impl Iterator<Item = DirEntry> {
    let mut walk = WalkDir::new(path);
    if skip_root {
        walk = walk.min_depth(1);
    }
    walk.into_iter().filter_entry(is_not_hidden).filter_map(|v| v.ok())
}

// Returns true if entry is a hidden filesystem entity.
pub(crate) fn is_not_hidden(entry: &DirEntry) -> bool {
    entry.file_name().to_str().map(|s| entry.depth() == 0 || !s.starts_with(".")).unwrap_or(false)
}

/// Generates a directory tree by executing handlebars templates against a set of variables.
pub(crate) struct CodeGenerator {
    handlebars: handlebars::Handlebars<'static>,
    template_files: Vec<TemplateFile>,
    template_vars: std::collections::BTreeMap<String, String>,
}

impl CodeGenerator {
    pub(crate) fn new() -> Self {
        let mut code_generator = Self {
            handlebars: Handlebars::new(),
            template_files: vec![],
            template_vars: std::collections::BTreeMap::new(),
        };
        code_generator.install_handlebars_helpers();
        code_generator
    }

    /// Adds a `TemplateFile` that will be generated into an ouptut file when
    /// [`CodeGenerator::generate`] is called.
    ///
    /// To create the TemplateFile, use [`hbrs_template_file`].
    pub(crate) fn with_template(mut self, template_file: TemplateFile) -> Self {
        self.template_files.push(template_file);
        self
    }

    /// Adds a template variable to use when generating code.
    ///
    /// The variable will be available for use by all templates when code is generated.
    pub(crate) fn with_template_var(
        mut self,
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        self.template_vars.insert(name.into(), value.into());
        self
    }

    /// Generates code and writes all files and directories to disk.
    pub(crate) fn generate<P: AsRef<Path>>(self, path: P) -> Result<(), Error> {
        // Generate code in a staging directory in case of failure.
        let tmp_dir = TempDir::new()?;
        for TemplateFile { filename, contents } in self.template_files {
            let code = self.handlebars.render_template(&contents, &self.template_vars)?;
            let output_path = tmp_dir.path().join(filename);
            file_write(output_path, &code)?;
        }

        // Commit changes.
        dir_create_if_absent(&path)?;
        dir_copy(tmp_dir.path(), &path)?;
        Ok(())
    }

    fn install_handlebars_helpers(&mut self) {
        // Returns the current year.
        handlebars_helper!(helper_year: |*args| {
            let _ = args;
            format!("{}", chrono::Utc::now().year())
        });

        self.handlebars.register_helper("year", Box::new(helper_year));
    }
}
