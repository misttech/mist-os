// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/// `Role` defines grouping for responsibilities on the policy message hub.
#[derive(PartialEq, Copy, Clone, Debug, Eq, Hash)]
pub enum Role {
    /// This role indicates that the messenger handles and enacts policy requests.
    PolicyHandler,
}
