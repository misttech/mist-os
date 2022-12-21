// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::state::PasswordError;

const PASSWORD_MINIMUM_LENGTH: u8 = 8;

/// Verify that a given password meets policy constraints.
pub fn check(password: &str) -> Result<(), PasswordError> {
    if password.len() < PASSWORD_MINIMUM_LENGTH.into() {
        return Err(PasswordError::TooShort(PASSWORD_MINIMUM_LENGTH));
    }
    Ok(())
}

#[cfg(test)]
mod test {
    use {super::*, assert_matches::assert_matches};

    #[fuchsia::test]
    fn too_short() {
        assert_matches!(check("short"), Err(PasswordError::TooShort(PASSWORD_MINIMUM_LENGTH)));
    }

    #[fuchsia::test]
    fn just_right() {
        assert_matches!(check("EqualLen"), Ok(()));
    }
}
