// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![deny(missing_docs)]

//! A Manager for downloading and storing assembly artifacts.

mod artifact;
mod artifact_cache;

pub use artifact::Artifact;
pub use artifact_cache::ArtifactCache;
