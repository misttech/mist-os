// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub struct SecurityServer;

impl SecurityServer {
    pub fn new() -> SecurityServer {
        // TODO(http://b/304732283): initialize the access vector cache.
        SecurityServer {}
    }
}
