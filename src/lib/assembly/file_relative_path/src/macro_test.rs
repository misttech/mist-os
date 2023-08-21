// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use assembly_file_relative_path::{FileRelativePathBuf, SupportsFileRelativePaths};
use serde::{Deserialize, Serialize};

// Needed by SupportsFileRelativePaths implementation
use anyhow as _;
use camino as _;

#[derive(SupportsFileRelativePaths, Debug, Deserialize, Serialize, PartialEq)]
struct SimpleStruct {
    flag: bool,
    path: FileRelativePathBuf,
}

#[test]
fn test_simple_struct() {
    let json = serde_json::json!({
      "flag": true,
      "path": "foo/file_1.json"
    });

    // The parsed file uses "file-relative" paths
    let parsed: SimpleStruct = serde_json::from_value(json).unwrap();
    assert_eq!(parsed.path, FileRelativePathBuf::FileRelative("foo/file_1.json".into()));

    // After resolving, they will be "resolved" paths
    let resolved = parsed.resolve_paths_from_file("contents/manifest.list").unwrap();
    assert_eq!(resolved.path, FileRelativePathBuf::Resolved("contents/foo/file_1.json".into()));

    // When created from paths/strings, they will start as "resolved"
    let created = SimpleStruct { flag: false, path: "resources/bar/file_2.json".into() };
    assert_eq!(created.path, FileRelativePathBuf::Resolved("resources/bar/file_2.json".into()));

    // And then can be made relative to a file or directory
    let relative = created.make_paths_relative_to_file("resources/manifest").unwrap();
    assert_eq!(relative.path, FileRelativePathBuf::FileRelative("bar/file_2.json".into()));
}

#[derive(SupportsFileRelativePaths, Debug, Deserialize, Serialize, PartialEq)]
struct ParentStruct {
    name: String,
    some_path: FileRelativePathBuf,

    // The 'file_relative_paths' attribute is needed by the 'SupportsFileRelativePaths' derive
    // macro so that it knows that this field has file-relative paths, and also implements the
    // 'SupportsFileRelativePaths' trait.
    #[file_relative_paths]
    child: SimpleStruct,

    // WARNING: This field isn't marked, and so it doesn't get included in the
    // 'SupportsFileRelativePaths' implementation created by the derive macro.
    bad_child: SimpleStruct,
}

#[test]
fn test_nested_struct() {
    let json = serde_json::json!({
      "name": "parent",
      "some_path": "baz/file_2.json",
      "child": {
        "flag": true,
        "path": "foo/file_1.json"
      },
      "bad_child": {
        "flag": false,
        "path": "this/will/not/change"
      }
    });

    // The parsed file uses "file-relative" paths
    let parsed: ParentStruct = serde_json::from_value(json).unwrap();
    assert_eq!(parsed.some_path, FileRelativePathBuf::FileRelative("baz/file_2.json".into()));
    assert_eq!(parsed.child.path, FileRelativePathBuf::FileRelative("foo/file_1.json".into()));
    assert_eq!(
        parsed.bad_child.path,
        FileRelativePathBuf::FileRelative("this/will/not/change".into())
    );

    // After resolving, they will be "resolved" paths, including the child structures, so long as
    // they are marked with the
    let resolved = parsed.resolve_paths_from_file("contents/manifest.list").unwrap();
    assert_eq!(
        resolved.some_path,
        FileRelativePathBuf::Resolved("contents/baz/file_2.json".into())
    );
    assert_eq!(
        resolved.child.path,
        FileRelativePathBuf::Resolved("contents/foo/file_1.json".into())
    );
    // WARNING: The field without an attribute is still file-relative!
    assert_eq!(
        resolved.bad_child.path,
        FileRelativePathBuf::FileRelative("this/will/not/change".into())
    );

    // When created from paths/strings, they will start as "resolved"
    let created = ParentStruct {
        name: "different parent".into(),
        some_path: "resources/bar/file_3.json".into(),
        child: SimpleStruct { flag: false, path: "resources/bar/file_2.json".into() },
        bad_child: SimpleStruct { flag: true, path: "resources/will/not/change".into() },
    };
    assert_eq!(
        created.some_path,
        FileRelativePathBuf::Resolved("resources/bar/file_3.json".into())
    );
    assert_eq!(
        created.child.path,
        FileRelativePathBuf::Resolved("resources/bar/file_2.json".into())
    );
    assert_eq!(
        created.bad_child.path,
        FileRelativePathBuf::Resolved("resources/will/not/change".into())
    );

    // And then can be made relative to a file or directory
    let relative = created.make_paths_relative_to_file("resources/manifest").unwrap();
    assert_eq!(relative.some_path, FileRelativePathBuf::FileRelative("bar/file_3.json".into()));
    assert_eq!(relative.child.path, FileRelativePathBuf::FileRelative("bar/file_2.json".into()));
    // WARNING: The field without an attribute is still resolved!
    assert_eq!(
        relative.bad_child.path,
        FileRelativePathBuf::Resolved("resources/will/not/change".into())
    );
}

#[derive(Deserialize, SupportsFileRelativePaths)]
struct StructWithOption {
    #[file_relative_paths]
    optional: Option<SimpleStruct>,
}

#[test]
fn test_struct_with_option() {
    let json = serde_json::json!({
        "optional": {
            "flag": true,
            "path": "foo/file_1.json"
          },
    });

    let parsed: StructWithOption = serde_json::from_value(json).unwrap();
    assert_eq!(
        parsed.optional,
        Some(SimpleStruct {
            flag: true,
            path: FileRelativePathBuf::FileRelative("foo/file_1.json".into())
        })
    );

    let resolved = parsed.resolve_paths_from_file("some/file").unwrap();
    assert_eq!(
        resolved.optional.unwrap().path,
        FileRelativePathBuf::Resolved("some//foo/file_1.json".into())
    );
}
