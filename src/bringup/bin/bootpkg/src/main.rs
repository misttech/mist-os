// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[fuchsia_async::run_singlethreaded]
async fn main() -> anyhow::Result<()> {
    let args: bootpkg::args::Args = argh::from_env();
    let boot_dir = fuchsia_fs::directory::open_in_namespace_deprecated(
        "/boot",
        fuchsia_fs::OpenFlags::RIGHT_READABLE | fuchsia_fs::OpenFlags::DIRECTORY,
    )?;
    bootpkg::bootpkg(boot_dir, args).await
}
