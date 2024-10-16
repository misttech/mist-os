// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::common::crypto::unlock_device;
use crate::common::{is_locked, MISSING_CREDENTIALS};
use crate::file_resolver::FileResolver;
use anyhow::Result;
use errors::ffx_bail;
use ffx_fastboot_interface::fastboot_interface::FastbootInterface;
use std::io::Write;

const UNLOCKED: &str = "Target is now unlocked.";
const UNLOCKED_ERR: &str = "Target is already unlocked.";

pub async fn unlock<W: Write, F: FileResolver + Sync, T: FastbootInterface>(
    writer: &mut W,
    file_resolver: &mut F,
    credentials: &Vec<String>,
    fastboot_interface: &mut T,
) -> Result<()> {
    if !is_locked(fastboot_interface).await? {
        ffx_bail!("{}", UNLOCKED_ERR);
    }

    if credentials.len() == 0 {
        ffx_bail!("{}", MISSING_CREDENTIALS);
    }

    unlock_device(writer, file_resolver, credentials, fastboot_interface).await?;
    writeln!(writer, "{}", UNLOCKED)?;
    Ok(())
}

////////////////////////////////////////////////////////////////////////////////
// tests

#[cfg(test)]
mod test {
    use super::*;
    use crate::common::vars::LOCKED_VAR;
    use crate::file_resolver::resolvers::EmptyResolver;
    use crate::test::setup;

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_unlocked_device_throws_err() -> Result<()> {
        let (state, mut proxy) = setup();
        {
            let mut state = state.lock().unwrap();
            // is_locked
            state.set_var(LOCKED_VAR.to_string(), "no".to_string());
        }
        let mut writer = Vec::<u8>::new();
        let result =
            unlock(&mut writer, &mut EmptyResolver::new()?, &vec!["test".to_string()], &mut proxy)
                .await;
        assert!(result.is_err());
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_missing_creds_throws_err() -> Result<()> {
        let (state, mut proxy) = setup();
        {
            let mut state = state.lock().unwrap();
            // is_locked
            state.set_var(LOCKED_VAR.to_string(), "yes".to_string());
        }
        let mut writer = Vec::<u8>::new();
        let result = unlock(&mut writer, &mut EmptyResolver::new()?, &vec![], &mut proxy).await;
        assert!(result.is_err());
        Ok(())
    }
}
