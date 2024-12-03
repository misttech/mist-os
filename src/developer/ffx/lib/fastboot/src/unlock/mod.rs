// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::common::crypto::unlock_device;
use crate::common::{is_locked, MISSING_CREDENTIALS};
use crate::file_resolver::FileResolver;
use crate::util;
use crate::util::Event;
use anyhow::Result;
use errors::ffx_bail;
use ffx_fastboot_interface::fastboot_interface::FastbootInterface;
use tokio::sync::mpsc::Sender;

const UNLOCKED_ERR: &str = "Target is already unlocked.";

pub async fn unlock<F: FileResolver + Sync, T: FastbootInterface>(
    messages: Sender<Event>,
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

    unlock_device(&messages, file_resolver, credentials, fastboot_interface).await?;
    messages.send(util::Event::Unlock(util::UnlockEvent::Done)).await?;
    Ok(())
}

////////////////////////////////////////////////////////////////////////////////
// tests

#[cfg(test)]
mod test {
    use tokio::sync::mpsc;

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
        let (client, _server) = mpsc::channel(1);
        let result =
            unlock(client, &mut EmptyResolver::new()?, &vec!["test".to_string()], &mut proxy).await;
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
        let (client, _server) = mpsc::channel(1);
        let result = unlock(client, &mut EmptyResolver::new()?, &vec![], &mut proxy).await;
        assert!(result.is_err());
        Ok(())
    }
}
