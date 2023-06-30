// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    delivery_blob::{compression::ChunkedArchiveError, DeliveryBlobError},
    fuchsia_zircon::Status,
    fxfs::{errors::FxfsError, log::*},
};

pub fn map_to_status(err: anyhow::Error) -> Status {
    if let Some(status) = err.root_cause().downcast_ref::<Status>() {
        status.clone()
    } else if let Some(fxfs_error) = err.root_cause().downcast_ref::<FxfsError>() {
        fxfs_error.clone().into()
    } else if let Some(delivery_blob_error) = err.root_cause().downcast_ref::<DeliveryBlobError>() {
        delivery_blob_error.clone().into()
    } else if let Some(_) = err.root_cause().downcast_ref::<ChunkedArchiveError>() {
        Status::IO_DATA_INTEGRITY
    } else {
        // Print the internal error if we re-map it because we will lose any context after this.
        warn!("Internal error: {:?}", err);
        Status::INTERNAL
    }
}
