// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use lazy_static::lazy_static;
use std::collections::HashMap;

pub const TEST_ROOT_REALM_NAME: &'static str = "test_root";
pub const TEST_ROOT_COLLECTION: &'static str = "test";
pub const WRAPPER_REALM_NAME: &'static str = "test_wrapper";
pub const HERMETIC_RESOLVER_REALM_NAME: &'static str = "hermetic_resolver";

pub const KERNEL_DEBUG_DATA_FOR_SCP: &'static str = "/tmp/kernel_debug";
pub const DEBUG_DATA_FOR_SCP: &'static str = "/tmp/debug";
pub const ISOLATED_TMP: &'static str = "/tmp/isolated";

pub const CUSTOM_ARTIFACTS_CAPABILITY_NAME: &'static str = "custom_artifacts";

// TODO(fxbug.dev/100034): Delete these once we no longer need to hard code these in the code.
pub const TEST_ENVIRONMENT_NAME: &'static str = "test-env";
pub const HERMETIC_TESTS_COLLECTION: &'static str = "tests";
pub const STARNIX_TESTS_COLLECTION: &'static str = "starnix-tests";
pub const SYSTEM_TESTS_COLLECTION: &'static str = "system-tests";
pub const CTS_TESTS_COLLECTION: &'static str = "cts-tests";
pub const VULKAN_TESTS_COLLECTION: &'static str = "vulkan-tests";
pub const CHROMIUM_TESTS_COLLECTION: &'static str = "chromium-tests";
pub const CHROMIUM_SYSTEM_TESTS_COLLECTION: &'static str = "chromium-system-tests";
pub const GOOGLE_TESTS_COLLECTION: &'static str = "google-tests";
pub const SYSTEM_VALIDATION_COLLECTION: &'static str = "system-validation-tests";

lazy_static! {
    pub static ref TEST_TYPE_REALM_MAP: HashMap<&'static str, &'static str> = [
        ("hermetic", HERMETIC_TESTS_COLLECTION),
        ("chromium", CHROMIUM_TESTS_COLLECTION),
        ("chromium-system", CHROMIUM_SYSTEM_TESTS_COLLECTION),
        ("cts", CTS_TESTS_COLLECTION),
        ("google", GOOGLE_TESTS_COLLECTION),
        ("starnix", STARNIX_TESTS_COLLECTION),
        ("system", SYSTEM_TESTS_COLLECTION),
        ("system-validation", SYSTEM_VALIDATION_COLLECTION),
        ("vulkan", VULKAN_TESTS_COLLECTION),
    ]
    .iter()
    .copied()
    .collect();
}
