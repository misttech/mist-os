// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::writer::Error as WriterError;
use diagnostics_hierarchy::Error as HierarchyError;
use inspect_format::{BlockIndex, BlockType, Error as FormatError};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ReaderError {
    #[error("FIDL error")]
    Fidl(#[source] anyhow::Error),

    #[error("Lazy node callback failed")]
    LazyCallback(#[source] anyhow::Error),

    #[error("expected header block on index 0")]
    MissingHeader,

    #[error("Cannot find parent block index {0}")]
    ParentIndexNotFound(BlockIndex),

    #[error("Malformed tree, no complete node with parent=0")]
    MalformedTree,

    #[error("VMO format error")]
    VmoFormat(#[source] FormatError),

    #[error("Tried to read more slots than available at block index {0}")]
    AttemptedToReadTooManyArraySlots(BlockIndex),

    #[error("unexpected array entry type format: {0:?}")]
    UnexpectedArrayEntryFormat(BlockType),

    #[error("Failed to parse name at index {0}")]
    ParseName(BlockIndex),

    #[error("Failed to get link content at index {0}")]
    GetLinkContent(BlockIndex),

    #[error("Failed to get extent at index {0}")]
    GetExtent(BlockIndex),

    #[error("Failed to get consistent snapshot")]
    InconsistentSnapshot,

    #[error("Header missing or is locked")]
    MissingHeaderOrLocked,

    #[error("Cannot read from no-op Inspector")]
    NoOpInspector,

    #[cfg(target_os = "fuchsia")]
    #[error("Failed to call vmo")]
    Vmo(fuchsia_zircon::Status),

    #[cfg(not(target_os = "fuchsia"))]
    #[error("Failed to call vmo")]
    Vmo(()),

    #[error("Error creating node hierarchy")]
    Hierarchy(#[source] HierarchyError),

    #[error("Failed to duplicate vmo handle")]
    DuplicateVmo,

    #[error("Failed to fetch vmo from Tree content")]
    FetchVmo,

    #[error("Failed to load tree name {0}")]
    FailedToLoadTree(String),

    #[error("Timed out reading tree")]
    TreeTimedOut,

    #[error("Failed to lock inspector state")]
    FailedToLockState(#[source] WriterError),

    #[error("Offset out of bounds while reading")]
    OffsetOutOfBounds,
}

impl From<FormatError> for ReaderError {
    fn from(error: FormatError) -> Self {
        Self::VmoFormat(error)
    }
}
