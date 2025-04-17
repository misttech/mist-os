// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// This module is used to house common "global" configuration values that may
// cross multiple crates, plugins or tools so as to avoid large,
// cross-binary dependency graphs.

/// The default target to communicate with if no target is specified.
pub const TARGET_DEFAULT_KEY: &str = "target.default";

/// TODO(https://fxbug.dev/394619603): This is the experimental feature flag
/// limiting target specification to explicit `--target` args and
/// `$FUCHSIA_NODENAME`/`$FUCHSIA_DEVICE_ADDR` environment variables.
pub const STATELESS_DEFAULT_TARGET_CONFIGURATION: &str = "target.stateless_default_configuration";
