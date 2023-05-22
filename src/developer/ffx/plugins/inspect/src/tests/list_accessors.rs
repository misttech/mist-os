// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::run_command;
use crate::tests::utils::{setup_fake_archive_accessor, setup_fake_rcs};
use ffx_writer::{Format, MachineWriter, TestBuffers};
use iquery::commands::ListAccessorsCommand;

#[fuchsia::test]
async fn test_list_accessors() {
    let test_buffers = TestBuffers::default();
    let mut writer = MachineWriter::new_test(Some(Format::Json), &test_buffers);
    let cmd = ListAccessorsCommand {};
    run_command(
        setup_fake_rcs(),
        setup_fake_archive_accessor(vec![]),
        ListAccessorsCommand::from(cmd),
        &mut writer,
    )
    .await
    .unwrap();

    let expected = serde_json::to_string(&vec![
        String::from("example/component:expose:fuchsia.diagnostics.ArchiveAccessor"),
        String::from("foo/bar/thing:expose:fuchsia.diagnostics.FeedbackArchiveAccessor"),
        String::from("foo/component:expose:fuchsia.diagnostics.FeedbackArchiveAccessor"),
    ])
    .unwrap();
    let output = test_buffers.into_stdout_str();
    assert_eq!(output.trim_end(), expected);
}
