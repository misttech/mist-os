// Copyright 2024 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(dead_code)]
#![allow(unused_imports)]
use anyhow::{Context, Error};
use fidl::endpoints::Proxy;
use fidl::AsHandleRef;
use fidl_fuchsia_io;
use fidl_fuchsia_tee;
use fuchsia_component::client::{connect_to_protocol_at, connect_to_protocol_at_path};
use fuchsia_fs;
use fuchsia_zircon::{self as zx};

#[fuchsia::test]
async fn connect_panic_ta() -> Result<(), Error> {
    const PANIC_TA_UUID: &str = "7672c06d-f8b3-482b-b8e2-f88fcc8604d7";
    let ta_dir = connect_to_protocol_at_path::<fidl_fuchsia_io::DirectoryMarker>("/ta")
        .context("Failed to connect to ta directory")?;
    let entries = fuchsia_fs::directory::readdir(&ta_dir).await?;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, PANIC_TA_UUID);
    assert_eq!(entries[0].kind, fuchsia_fs::directory::DirentKind::Directory);
    let panic_ta = connect_to_protocol_at::<fidl_fuchsia_tee::ApplicationMarker>(
        "/ta/".to_owned() + PANIC_TA_UUID,
    )?;
    let result = panic_ta.open_session2(vec![]).await;
    // We expect the panic TA to panic when send it the first request which will close the channel.
    assert!(result.is_err());
    let channel = panic_ta.as_channel();
    let wait_result = channel.wait_handle(
        zx::Signals::CHANNEL_PEER_CLOSED | zx::Signals::CHANNEL_READABLE,
        zx::Time::INFINITE,
    );
    assert_eq!(wait_result, Ok(zx::Signals::CHANNEL_PEER_CLOSED));

    Ok(())
}
