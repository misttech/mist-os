// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::u64_le;

/// FIDL alignment, used for buffer alignment to ensure decoding in-place is
/// possible.
pub const CHUNK_SIZE: usize = 8;

/// A group of eight bytes, aligned to an 8-byte boundary.
pub type Chunk = u64_le;
