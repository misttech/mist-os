// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod simple8b_rle;
mod uncompressed;

pub(crate) use simple8b_rle::Simple8bRleRingBuffer;
pub(crate) use uncompressed::UncompressedRingBuffer;
