// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use errors::ffx_bail;
use ffx_reboot_args::RebootCommand;
use fho::{FfxContext, FfxMain, FfxTool, SimpleWriter};
use fidl_fuchsia_developer_ffx::{TargetProxy, TargetRebootError, TargetRebootState};

const NETSVC_NOT_FOUND: &str = "The Fuchsia target's netsvc address could not be determined.\n\
                                If this problem persists, try running `ffx doctor` for diagnostics";
const NETSVC_COMM_ERR: &str = "There was a communication error using netsvc to reboot.\n\
                               If the problem persists, try running `ffx doctor` for further diagnostics";
const BOOT_TO_ZED: &str = "Cannot reboot from Bootloader state to Recovery state.";
const REBOOT_TO_PRODUCT: &str = "\nReboot to Product state with `ffx target reboot` and try again.";
const COMM_ERR: &str = "There was a communication error with the device. Please try again. \n\
                        If the problem persists, try running `ffx doctor` for further diagnostics";

#[derive(FfxTool)]
pub struct RebootTool {
    #[command]
    cmd: RebootCommand,
    target_proxy: TargetProxy,
}

fho::embedded_plugin!(RebootTool);

#[async_trait(?Send)]
impl FfxMain for RebootTool {
    type Writer = SimpleWriter;
    async fn main(self, _writer: Self::Writer) -> fho::Result<()> {
        reboot(self.target_proxy, self.cmd).await
    }
}

async fn reboot(target_proxy: TargetProxy, cmd: RebootCommand) -> fho::Result<()> {
    let state = reboot_state(&cmd)?;
    match target_proxy.reboot(state).await.bug()? {
        Ok(_) => Ok(()),
        Err(TargetRebootError::NetsvcCommunication) => {
            ffx_bail!("{}", NETSVC_COMM_ERR)
        }
        Err(TargetRebootError::NetsvcAddressNotFound) => {
            ffx_bail!("{}", NETSVC_NOT_FOUND)
        }
        Err(TargetRebootError::FastbootToRecovery) => {
            ffx_bail!("{}{}", BOOT_TO_ZED, REBOOT_TO_PRODUCT)
        }
        Err(TargetRebootError::TargetCommunication)
        | Err(TargetRebootError::FastbootCommunication) => ffx_bail!("{}", COMM_ERR),
    }
}

fn reboot_state(cmd: &RebootCommand) -> fho::Result<TargetRebootState> {
    match (cmd.bootloader, cmd.recovery) {
        (true, true) => {
            ffx_bail!("Cannot specify booth bootloader and recovery switches at the same time.")
        }
        (true, false) => Ok(TargetRebootState::Bootloader),
        (false, true) => Ok(TargetRebootState::Recovery),
        (false, false) => Ok(TargetRebootState::Product),
    }
}

////////////////////////////////////////////////////////////////////////////////
// tests
#[cfg(test)]
mod test {
    use super::*;
    use fidl_fuchsia_developer_ffx::TargetRequest;

    fn setup_fake_target_server(cmd: RebootCommand) -> TargetProxy {
        fho::testing::fake_proxy(move |req| match req {
            TargetRequest::Reboot { state: _, responder } => {
                assert!(!(cmd.bootloader && cmd.recovery));
                responder.send(Ok(())).unwrap();
            }
            r => panic!("unexpected request: {:?}", r),
        })
    }

    async fn run_reboot_test(cmd: RebootCommand) -> fho::Result<()> {
        let target_proxy = setup_fake_target_server(cmd);
        reboot(target_proxy, cmd).await
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_reboot() -> fho::Result<()> {
        run_reboot_test(RebootCommand { bootloader: false, recovery: false }).await
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_bootloader() -> fho::Result<()> {
        run_reboot_test(RebootCommand { bootloader: true, recovery: false }).await
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_recovery() -> fho::Result<()> {
        run_reboot_test(RebootCommand { bootloader: false, recovery: true }).await
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_error() {
        assert!(run_reboot_test(RebootCommand { bootloader: true, recovery: true }).await.is_err())
    }
}
