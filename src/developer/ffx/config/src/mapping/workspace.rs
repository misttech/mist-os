// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::mapping::{postprocess, preprocess, replace_regex};
use crate::EnvironmentContext;
use lazy_static::lazy_static;
use regex::Regex;
use serde_json::Value;
use std::path::Path;

fn find_workspace_root(mut current: &Path) -> Option<&Path> {
    loop {
        if current.join("WORKSPACE").exists() || current.join("WORKSPACE.bazel").exists() {
            return Some(current);
        } else {
            current = current.parent()?;
        }
    }
}

/// Replaces `$FIND_WORKSPACE_ROOT` with the path to the nearest parent directory
/// with a WORKSPACE or WORKSPACE.bazel file in it. If it doesn't find one,
/// this value will be skipped.
pub(crate) fn workspace(ctx: &EnvironmentContext, value: Value) -> Option<Value> {
    lazy_static! {
        static ref REGEX: Regex = Regex::new(r"\$(FIND_WORKSPACE_ROOT)").unwrap();
    }

    let Some(s) = preprocess(&value).filter(|s| REGEX.is_match(s)) else { return Some(value) };
    let workspace_root = find_workspace_root(ctx.project_root()?).and_then(Path::to_str)?;
    Some(postprocess(replace_regex(&s, &*REGEX, |_| Ok(workspace_root.to_owned()))))
}

////////////////////////////////////////////////////////////////////////////////
// tests
#[cfg(test)]
mod test {
    use std::fs::File;

    use tempfile::{tempdir, TempDir};

    use crate::environment::ExecutableKind;
    use ffx_config_domain::ConfigDomain;

    use super::*;

    fn make_temp_workspace_dir(workspace_filename: Option<&str>) -> TempDir {
        let workspace = tempdir().expect("temp directory");
        if let Some(name) = workspace_filename {
            File::create(workspace.path().join(name)).expect("workspace file");
        }
        workspace
    }

    fn make_test_env(workspace_dir: &TempDir) -> EnvironmentContext {
        let config_domain = ConfigDomain::load_from_contents(
            workspace_dir.path().join("fuchsia_env.toml").try_into().unwrap(),
            Default::default(),
        )
        .unwrap();
        EnvironmentContext::config_domain(
            ExecutableKind::Test,
            config_domain,
            Default::default(),
            Some(workspace_dir.path().to_owned()),
        )
    }

    #[test]
    fn test_find_workspace() {
        for filename in ["WORKSPACE", "WORKSPACE.bazel"] {
            let workspace_dir = make_temp_workspace_dir(Some(filename));
            assert_eq!(
                None,
                find_workspace_root(workspace_dir.path().parent().unwrap()),
                "Shouldn't find workspace in outer directory"
            );
            assert_eq!(
                Some(workspace_dir.path()),
                find_workspace_root(workspace_dir.path()),
                "Find workspace in the root"
            );

            std::fs::create_dir_all(workspace_dir.path().join("first/second/third")).unwrap();
            for subdir in ["first", "first/second", "first/second/third"] {
                let subdir = workspace_dir.path().join(subdir);
                assert_eq!(
                    Some(workspace_dir.path()),
                    find_workspace_root(&subdir),
                    "Find workspace in a subdirectory"
                );
            }
        }
    }

    #[test]
    fn test_mapper_no_workspace() {
        let workspace_dir = make_temp_workspace_dir(None);
        let ctx = make_test_env(&workspace_dir);
        let test = Value::String("$FIND_WORKSPACE_ROOT".to_string());
        assert_eq!(workspace(&ctx, test), None);
    }

    #[test]
    fn test_mapper() {
        let workspace_dir = make_temp_workspace_dir(Some("WORKSPACE"));
        let ctx = make_test_env(&workspace_dir);
        let value = workspace_dir.path().to_string_lossy().to_string();
        let test = Value::String("$FIND_WORKSPACE_ROOT".to_string());
        assert_eq!(workspace(&ctx, test), Some(Value::String(value)));
    }

    #[test]
    fn test_mapper_multiple() {
        let workspace_dir = make_temp_workspace_dir(Some("WORKSPACE"));
        let ctx = make_test_env(&workspace_dir);
        let value = workspace_dir.path().to_string_lossy().to_string();
        let test = Value::String("$FIND_WORKSPACE_ROOT/$FIND_WORKSPACE_ROOT".to_string());
        assert_eq!(workspace(&ctx, test), Some(Value::String(format!("{}/{}", value, value))));
    }

    #[test]
    fn test_mapper_returns_pass_through() {
        let workspace_dir = make_temp_workspace_dir(Some("WORKSPACE"));
        let ctx = make_test_env(&workspace_dir);
        let test = Value::String("$WHATEVER".to_string());
        assert_eq!(workspace(&ctx, test), Some(Value::String("$WHATEVER".to_string())));
    }
}
