// Copyright 2024 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(dead_code)]
#![allow(unused_imports)]
use anyhow::{Context, Error};
use fidl::endpoints::Proxy;
use fidl::AsHandleRef;
use fidl_fuchsia_tee::ReturnOrigin;
use fuchsia_component::client::{connect_to_protocol_at, connect_to_protocol_at_path};
use fuchsia_zircon::{self as zx};
use tee_internal::binding::TEE_ERROR_TARGET_DEAD;
use {fidl_fuchsia_io, fidl_fuchsia_tee, fuchsia_fs};

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
    assert!(result.is_ok());

    // We expect the panic TA to panic when send it the first request.
    if let Ok((_, op_result)) = result {
        assert_eq!(op_result.return_code, Some(TEE_ERROR_TARGET_DEAD as u64));
        assert_eq!(op_result.return_origin, Some(ReturnOrigin::TrustedOs));
        assert_eq!(op_result.parameter_set, None);
    }

    // Subsequent operations on the same connection should also fail with TARGET_DEAD.
    let result = panic_ta.invoke_command(0, 0, vec![]).await;
    assert!(result.is_ok());
    if let Ok(op_result) = result {
        assert_eq!(op_result.return_code, Some(TEE_ERROR_TARGET_DEAD as u64));
        assert_eq!(op_result.return_origin, Some(ReturnOrigin::TrustedOs));
        assert_eq!(op_result.parameter_set, None);
    }
    Ok(())
}
