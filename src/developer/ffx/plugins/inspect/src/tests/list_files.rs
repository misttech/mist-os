// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    run_command,
    tests::utils::{setup_fake_archive_accessor, setup_fake_rcs},
};
use assert_matches::assert_matches;
use ffx_writer::{Format, MachineWriter, TestBuffers};
use iquery::commands::{ListFilesCommand, ListFilesResultItem};

#[fuchsia::test]
async fn test_list_files_no_parameters() {
    let test_buffers = TestBuffers::default();
    let mut writer = MachineWriter::new_test(Some(Format::Json), &test_buffers);
    let cmd = ListFilesCommand { monikers: vec![] };
    run_command(
        setup_fake_rcs(),
        setup_fake_archive_accessor(vec![]),
        ListFilesCommand::from(cmd),
        &mut writer,
    )
    .await
    .unwrap();

    let expected = serde_json::to_string(&vec![
        ListFilesResultItem::new(
            "example/component".to_owned(),
            vec!["fuchsia.inspect.Tree".to_owned()],
        ),
        ListFilesResultItem::new(
            "other/component".to_owned(),
            vec!["fuchsia.inspect.Tree".to_owned()],
        ),
    ])
    .unwrap();
    let output = test_buffers.into_stdout_str();
    assert_eq!(output.trim_end(), expected);
}

#[fuchsia::test]
async fn test_list_files_with_valid_moniker() {
    let test_buffers = TestBuffers::default();
    let mut writer = MachineWriter::new_test(Some(Format::Json), &test_buffers);
    let cmd = ListFilesCommand { monikers: vec!["example/component".to_owned()] };
    run_command(
        setup_fake_rcs(),
        setup_fake_archive_accessor(vec![]),
        ListFilesCommand::from(cmd),
        &mut writer,
    )
    .await
    .unwrap();

    let expected = serde_json::to_string(&vec![ListFilesResultItem::new(
        "example/component".to_owned(),
        vec!["fuchsia.inspect.Tree".to_owned()],
    )])
    .unwrap();
    let output = test_buffers.into_stdout_str();
    assert_eq!(output.trim_end(), expected);
}

#[fuchsia::test]
async fn test_list_files_with_invalid_moniker() {
    let test_buffers = TestBuffers::default();
    let mut writer = MachineWriter::new_test(Some(Format::Json), &test_buffers);
    let cmd = ListFilesCommand { monikers: vec!["bah".to_owned()] };
    let result = run_command(
        setup_fake_rcs(),
        setup_fake_archive_accessor(vec![]),
        ListFilesCommand::from(cmd),
        &mut writer,
    )
    .await;

    assert_matches!(result, Err(_));
}
