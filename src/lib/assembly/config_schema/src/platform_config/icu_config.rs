// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde::{Deserialize, Serialize};

/// System assembly configuration for the ICU subsystem.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct ICUConfig {
    /// The revision (corresponding to a git commit ID) of the ICU library to
    /// use in system assembly. This revision is constrained to the commit IDs
    /// available in the repos at `//third_party/icu/{default,stable,latest}`,
    pub revision: Option<String>,
}
