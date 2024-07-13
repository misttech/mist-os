// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::identity::ComponentIdentity;
use moniker::ExtendedMoniker;
use once_cell::sync::Lazy;
use std::sync::Arc;

pub static TEST_IDENTITY: Lazy<Arc<ComponentIdentity>> = Lazy::new(|| {
    Arc::new(ComponentIdentity::new(
        ExtendedMoniker::parse_str("./fake-test-env/test-component").unwrap(),
        "fuchsia-pkg://fuchsia.com/testing123#test-component.cm",
    ))
});
