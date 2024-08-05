// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::identity::ComponentIdentity;
use moniker::ExtendedMoniker;
use std::sync::{Arc, LazyLock};

pub static TEST_IDENTITY: LazyLock<Arc<ComponentIdentity>> = LazyLock::new(|| {
    Arc::new(ComponentIdentity::new(
        ExtendedMoniker::parse_str("./fake-test-env/test-component").unwrap(),
        "fuchsia-pkg://fuchsia.com/testing123#test-component.cm",
    ))
});
