// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// This module is used to house common "global" configuration values that may
// cross multiple crates, plugins or tools so as to avoid large,
// cross-binary dependency graphs.

/// The default target to communicate with if no target is specified.
pub const TARGET_DEFAULT_KEY: &str = "target.default";
