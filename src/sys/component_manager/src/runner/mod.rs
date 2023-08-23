// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod builtin;
pub mod namespace;

pub use namespace::Entry as NamespaceEntry;
pub use namespace::{Namespace, NamespaceError};
