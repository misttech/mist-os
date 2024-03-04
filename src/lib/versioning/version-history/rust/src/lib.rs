// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub use version_history_shared::*;

const VERSIONS: &[Version] = &version_history_macro::declare_version_history!();

/// Global [VersionHistory] instance generated at build-time from the contents
/// of `//sdk/version_history.json`.
pub const HISTORY: VersionHistory = VersionHistory::new(VERSIONS);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_proc_macro_worked() {
        let expected = version_history_shared::version_history().unwrap();
        assert_eq!(expected, VERSIONS);
    }

    #[test]
    fn test_example_supported_abi_revision_is_supported() {
        let example_version = HISTORY.get_example_supported_version_for_tests();

        assert_eq!(
            HISTORY.check_api_level_for_build(example_version.api_level),
            Ok(example_version.abi_revision)
        );

        HISTORY
            .check_abi_revision_for_runtime(example_version.abi_revision)
            .expect("you said it was supported!");
    }
}
