// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::fastboot_interface::Fastboot;
use crate::fastboot_interface::FastbootInterface;
use crate::fastboot_interface::RebootEvent;
use crate::fastboot_interface::UploadProgress;
use crate::fastboot_interface::Variable;
use crate::interface_factory::InterfaceFactory;
use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use chrono::Duration;
use fastboot::{
    command::{ClientVariable, Command},
    download,
    reply::Reply,
    send, send_with_listener, send_with_timeout, upload, SendError,
};
use ffx_config::get;
use futures::io::{AsyncRead, AsyncWrite};
use std::fmt::Debug;
use std::fs::read;
use tokio::sync::mpsc::Sender;

///////////////////////////////////////////////////////////////////////////////
// FastbootProxy
//

#[derive(Debug)]
pub struct FastbootProxy<T: AsyncRead + AsyncWrite + Unpin> {
    #[allow(dead_code)]
    target_id: String,
    interface: Option<T>,
    interface_factory: Box<dyn InterfaceFactory<T>>,
}

/// The timeout rate in mb/s when communicating with the target device
const FLASH_TIMEOUT_RATE: &str = "fastboot.flash.timeout_rate";
/// The minimum flash timeout (in seconds) for flashing to a target device
const MIN_FLASH_TIMEOUT: &str = "fastboot.flash.min_timeout_secs";

fn handle_timeout_as_okay(r: Result<Reply>) -> Result<Reply> {
    match r {
        Err(e) if matches!(e.downcast_ref::<SendError>(), Some(SendError::Timeout)) => {
            tracing::debug!("Timed out waiting for bootloader response; assuming it's okay");
            Ok(Reply::Okay("".to_string()))
        }
        _ => r,
    }
}

impl<T: AsyncRead + AsyncWrite + Unpin + Debug> FastbootInterface for FastbootProxy<T> {}

#[derive(Debug)]
struct VariableListener(Sender<Variable>);

impl VariableListener {
    fn new(listener: Sender<Variable>) -> Result<Self> {
        Ok(Self(listener))
    }
}

#[async_trait]
impl fastboot::InfoListener for VariableListener {
    #[tracing::instrument]
    async fn on_info(&self, info: String) -> Result<()> {
        if let Some((name, val)) = info.split_once(':') {
            tracing::debug!("Got a variable string: {}", info);
            self.0.send(Variable { name: name.to_string(), value: val.to_string() }).await?;
        } else {
            tracing::warn!("Expected to get a variable string. Got: {}", info);
        }
        Ok(())
    }
}

#[derive(Debug)]
struct ProgressListener(Sender<UploadProgress>);

impl ProgressListener {
    fn new(listener: Sender<UploadProgress>) -> Result<Self> {
        Ok(Self(listener))
    }
}

#[async_trait]
impl fastboot::UploadProgressListener for ProgressListener {
    #[tracing::instrument]
    async fn on_started(&self, size: usize) -> Result<()> {
        self.0.send(UploadProgress::OnStarted { size: size.try_into()? }).await?;
        Ok(())
    }
    #[tracing::instrument]
    async fn on_progress(&self, bytes_written: u64) -> Result<()> {
        self.0.send(UploadProgress::OnProgress { bytes_written }).await?;
        Ok(())
    }
    #[tracing::instrument]
    async fn on_error(&self, error: &str) -> Result<()> {
        self.0.send(UploadProgress::OnError { error: anyhow!(error.to_string()) }).await?;
        Ok(())
    }
    #[tracing::instrument]
    async fn on_finished(&self) -> Result<()> {
        self.0.send(UploadProgress::OnFinished).await?;
        Ok(())
    }
}

impl<T: AsyncRead + AsyncWrite + Unpin + Debug> FastbootProxy<T> {
    pub fn new(
        target_id: String,
        interface: T,
        interface_factory: impl InterfaceFactory<T> + 'static,
    ) -> Self {
        Self {
            target_id,
            interface: Some(interface),
            interface_factory: Box::new(interface_factory),
        }
    }

    async fn reconnect(&mut self) -> Result<()> {
        // Explicitly here.
        self.interface = None;

        // Wait for it to show up again
        tracing::debug!("About to rediscover target");
        self.interface_factory.rediscover().await?;

        // Reconnect
        self.interface.replace(self.interface_factory.open().await?);
        tracing::warn!("Reconnected");
        Ok(())
    }

    async fn interface(&mut self) -> Result<&mut T> {
        if self.interface.is_none() {
            self.interface.replace(self.interface_factory.open().await?);
        }
        Ok(self.interface.as_mut().expect("interface interface not available"))
    }
}

#[async_trait(?Send)]
impl<T: AsyncRead + AsyncWrite + Unpin + Debug> Fastboot for FastbootProxy<T> {
    async fn prepare(&mut self, _listener: Sender<RebootEvent>) -> Result<()> {
        // TODO(colnnelson): This is a part of the original fastboot interface, and is
        // "wrong". The device needs to be in fastboot mode before now.
        // This function exists soely to reboot the target from
        // product/zedboot into fastboot mode.
        Ok(())
    }

    #[tracing::instrument]
    async fn get_var(&mut self, name: &str) -> Result<String> {
        send(Command::GetVar(ClientVariable::Oem(name.to_string())), self.interface().await?)
            .await
            .and_then(|r| match r {
                Reply::Okay(v) => Ok(v),
                Reply::Fail(s) => bail!("Failed to get {}: {}", name, s),
                _ => bail!("Unexpected reply from fastboot deviced for get_var"),
            })
    }

    #[tracing::instrument]
    async fn get_all_vars(&mut self, listener: Sender<Variable>) -> Result<()> {
        let variable_listener = VariableListener::new(listener)?;
        send_with_listener(
            Command::GetVar(ClientVariable::All),
            self.interface().await?,
            &variable_listener,
        )
        .await
        .and_then(|r| match r {
            Reply::Okay(_) => Ok(()),
            Reply::Fail(s) => bail!("Failed to get all variables {}", s),
            _ => bail!("Unexpected reply from fastboot deviced for get_var"),
        })
    }

    #[tracing::instrument]
    async fn flash(
        &mut self,
        partition_name: &str,
        path: &str,
        listener: Sender<UploadProgress>,
    ) -> Result<()> {
        let bytes = read(path)?;
        let progress_listener = ProgressListener::new(listener)?;
        let upload_reply = upload(&bytes[..], self.interface().await?, &progress_listener)
            .await
            .context(format!("uploading {}", path))?;
        match upload_reply {
            Reply::Okay(s) => tracing::debug!("Received response from download command: {}", s),
            Reply::Fail(s) => bail!("Failed to upload {}: {}", path, s),
            _ => bail!("Unexpected reply from fastboot device for download: {:?}", upload_reply),
        };
        //timeout rate is in mb per seconds
        let min_timeout: i64 = get(MIN_FLASH_TIMEOUT).await?;
        let timeout_rate: i64 = get(FLASH_TIMEOUT_RATE).await?;
        let megabytes = (bytes.len() / 1000000) as i64;
        let mut timeout = megabytes / timeout_rate;
        timeout = std::cmp::max(timeout, min_timeout);
        tracing::debug!("Estimated timeout: {}s for {}MB", timeout, megabytes);
        let span = tracing::span!(tracing::Level::INFO, "device_flash").entered();
        let send_reply = send_with_timeout(
            Command::Flash(partition_name.to_string()),
            self.interface().await?,
            Duration::seconds(timeout),
        )
        .await
        .context("sending flash");
        drop(span);
        match send_reply {
            Ok(Reply::Okay(_)) => Ok(()),
            Ok(Reply::Fail(s)) => bail!("Failed to flash \"{}\": {}", partition_name, s),
            Ok(_) => bail!("Unexpected reply from fastboot device for flash command"),
            Err(ref e) => {
                if let Some(ffx_err) = e.downcast_ref::<SendError>() {
                    match ffx_err {
                        SendError::Timeout => {
                            if timeout_rate == 1 {
                                bail!("Could not read response from device.  Reply timed out.");
                            }
                            let lowered_rate = timeout_rate - 1;
                            let timeout_err = format!(
                                "Time out while waiting on a response from the device. \n\
                                The current timeout rate is {} mb/s.  Try lowering the timeout rate: \n\
                                ffx config set \"{}\" {}",
                                timeout_rate, FLASH_TIMEOUT_RATE, lowered_rate
                            );
                            bail!("{}", timeout_err);
                        }
                    }
                }
                bail!("Unexpected reply from fastboot device for flash command: {:?}", send_reply)
            }
        }
    }

    #[tracing::instrument]
    async fn erase(&mut self, partition_name: &str) -> Result<()> {
        let reply = send(Command::Erase(partition_name.to_string()), self.interface().await?)
            .await
            .context("sending erase")?;
        match reply {
            Reply::Okay(_) => {
                tracing::debug!("Successfully erased parition: {}", partition_name);
                Ok(())
            }
            Reply::Fail(s) => bail!("Failed to erase \"{}\": {}", partition_name, s),
            _ => bail!("Unexpected reply from fastboot device for erase command: {:?}", reply),
        }
    }

    #[tracing::instrument]
    async fn boot(&mut self) -> Result<()> {
        // Note: the target may not successfully send a response when asked to boot,
        // so let's use a short time-out, and treat a timeout error as a success.
        let reply = handle_timeout_as_okay(
            send_with_timeout(Command::Boot, self.interface().await?, Duration::seconds(3)).await,
        )
        .context("sending boot")?;
        match reply {
            Reply::Okay(_) => {
                tracing::debug!("Successfully sent boot");
                Ok(())
            }
            Reply::Fail(s) => bail!("Failed to boot: {}", s),
            _ => bail!("Unexpected reply from fastboot device for boot command: {reply:?}"),
        }
    }

    #[tracing::instrument]
    async fn reboot(&mut self) -> Result<()> {
        // Note: the target may not successfully send a response when asked to reboot,
        // so let's use a short time-out, and treat a timeout error as a success.
        let reply = handle_timeout_as_okay(
            send_with_timeout(Command::Reboot, self.interface().await?, Duration::seconds(3)).await,
        )
        .context("sending reboot")?;
        match reply {
            Reply::Okay(_) => {
                tracing::debug!("Successfully sent reboot");
                Ok(())
            }
            Reply::Fail(s) => bail!("Failed to reboot: {}", s),
            _ => bail!("Unexpected reply from fastboot device for reboot command: {reply:?}"),
        }
    }

    #[tracing::instrument]
    async fn reboot_bootloader(&mut self, listener: Sender<RebootEvent>) -> Result<()> {
        // Note: the target may not successfully send a response when asked to reboot-bootloader,
        // so let's use a short time-out, and treat a timeout error as a success.
        let reply = handle_timeout_as_okay(
            send_with_timeout(
                Command::RebootBootLoader,
                self.interface().await?,
                Duration::seconds(3),
            )
            .await,
        )
        .context("sending reboot bootloader")?;
        match reply {
            Reply::Okay(_) => {
                tracing::debug!("Successfully sent reboot bootloader");
                let send_res = listener.send(RebootEvent::OnReboot).await;
                if send_res.is_err() {
                    tracing::debug!(
                        "reboot_bootloader hit error sending the reboot event to caller: {:#?}",
                        send_res
                    );
                }
            }
            Reply::Fail(s) => bail!("Failed to reboot to bootloader: {}", s),
            _ => {
                bail!("Unexpected reply from fastboot device for reboot bootloader command: {reply:?}")
            }
        };
        // Once the target is rebooted, reconnect
        self.reconnect().await.context("reconnecting after rebooting to bootloader")
    }

    #[tracing::instrument]
    async fn continue_boot(&mut self) -> Result<()> {
        // Note: the target may not successfully send a response when asked to continue,
        // so let's use a short time-out, and treat a timeout error as a success.
        let reply = handle_timeout_as_okay(
            send_with_timeout(Command::Continue, self.interface().await?, Duration::seconds(3))
                .await,
        )
        .context("sending continue")?;
        match reply {
            Reply::Okay(_) => {
                tracing::debug!("Successfully sent continue");
                Ok(())
            }
            Reply::Fail(s) => bail!("Failed to continue: {}", s),
            _ => bail!("Unexpected reply from fastboot device for continue command: {reply:?}"),
        }
    }

    #[tracing::instrument]
    async fn get_staged(&mut self, path: &str) -> Result<()> {
        match download(&path.to_string(), self.interface().await?)
            .await
            .context(format!("downloading to {}", path))?
        {
            Reply::Okay(_) => {
                tracing::debug!("Successfully downloaded to \"{}\"", path);
                Ok(())
            }
            Reply::Fail(s) => bail!("Failed to download: {}", s),
            _ => bail!("Unexpected reply from fastboot device for download command"),
        }
    }

    #[tracing::instrument]
    async fn stage(&mut self, path: &str, listener: Sender<UploadProgress>) -> Result<()> {
        let progress_listener = ProgressListener::new(listener)?;
        let bytes = read(path)?;
        tracing::debug!("uploading file size: {}", bytes.len());
        match upload(&bytes[..], self.interface().await?, &progress_listener)
            .await
            .context(format!("uploading {}", path))?
        {
            Reply::Okay(s) => {
                tracing::debug!("Received response from download command: {}", s);
                Ok(())
            }
            Reply::Fail(s) => bail!("Failed to upload {}: {}", path, s),
            _ => bail!("Unexpected reply from fastboot device for download"),
        }
    }

    #[tracing::instrument]
    async fn set_active(&mut self, slot: &str) -> Result<()> {
        match send(Command::SetActive(slot.to_string()), self.interface().await?)
            .await
            .context("sending set_active")?
        {
            Reply::Okay(_) => {
                tracing::debug!("Successfully sent set_active");
                Ok(())
            }
            Reply::Fail(s) => bail!("Failed to set_active: {}", s),
            _ => bail!("Unexpected reply from fastboot device for set_active command"),
        }
    }

    #[tracing::instrument]
    async fn oem(&mut self, command: &str) -> Result<()> {
        match send(Command::Oem(command.to_string()), self.interface().await?)
            .await
            .context("sending oem")?
        {
            Reply::Okay(_) => {
                tracing::debug!("Successfully sent oem command \"{}\"", command);
                Ok(())
            }
            Reply::Fail(s) => bail!("Failed to oem \"{}\": {}", command, s),
            _ => bail!("Unexpected reply from fastboot device for oem command \"{}\"", command),
        }
    }
}

////////////////////////////////////////////////////////////////////////////////
// tests

#[cfg(test)]
mod test {
    use super::*;
    use crate::interface_factory::InterfaceFactoryBase;
    use fastboot::test_transport::TestTransport;
    use pretty_assertions::assert_eq;
    use rand::{rngs::SmallRng, RngCore, SeedableRng};
    use std::io::Read;
    use std::io::Seek;
    use std::io::SeekFrom;
    use std::io::Write;
    use tempfile::{NamedTempFile, TempDir};
    use tokio::sync::mpsc;
    use tokio::sync::mpsc::Receiver;

    #[derive(Default, Debug, Clone)]
    struct TestTransportFactory {}

    #[async_trait(?Send)]
    impl InterfaceFactoryBase<TestTransport> for TestTransportFactory {
        async fn open(&mut self) -> Result<TestTransport> {
            Ok(TestTransport::new())
        }

        async fn close(&self) {}
    }

    impl InterfaceFactory<TestTransport> for TestTransportFactory {}

    ///////////////////////////////////////////////////////////////////////////
    //  get_var
    //

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_get_var() -> Result<()> {
        {
            let mut test_transport = TestTransport::new();
            test_transport.push(Reply::Okay("0.4".to_string()));
            let mut fastboot_client = FastbootProxy::<TestTransport> {
                target_id: "foo".to_string(),
                interface: Some(test_transport),
                interface_factory: Box::new(TestTransportFactory {}),
            };

            assert_eq!(fastboot_client.get_var(&"version").await?, "0.4");
        }
        {
            let mut test_transport = TestTransport::new();
            test_transport.push(Reply::Fail("variable doesnt exist".to_string()));
            let mut fastboot_client = FastbootProxy::<TestTransport> {
                target_id: "foo".to_string(),
                interface: Some(test_transport),
                interface_factory: Box::new(TestTransportFactory {}),
            };

            assert_eq!(fastboot_client.target_id, "foo");
            assert!(fastboot_client.get_var("version").await.is_err())
        }
        {
            let mut test_transport = TestTransport::new();
            test_transport.push(Reply::Data(1234));
            let mut fastboot_client = FastbootProxy::<TestTransport> {
                target_id: "foo".to_string(),
                interface: Some(test_transport),
                interface_factory: Box::new(TestTransportFactory {}),
            };

            assert_eq!(fastboot_client.target_id, "foo");
            assert!(fastboot_client.get_var("version").await.is_err())
        }
        Ok(())
    }

    ///////////////////////////////////////////////////////////////////////////
    //  get_all_vars
    //

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_get_all_vars() -> Result<()> {
        let (var_client, mut var_server): (Sender<Variable>, Receiver<Variable>) = mpsc::channel(3);
        let mut test_transport = TestTransport::new();
        test_transport.push(Reply::Okay("Done".to_string()));
        test_transport.push(Reply::Info("name:ianthe".to_string()));
        test_transport.push(Reply::Info("cav:babs".to_string()));
        test_transport.push(Reply::Info("sis:corona".to_string()));
        let mut fastboot_client = FastbootProxy::<TestTransport> {
            target_id: "foo".to_string(),
            interface: Some(test_transport),
            interface_factory: Box::new(TestTransportFactory {}),
        };

        let _ = fastboot_client.get_all_vars(var_client).await?;
        assert_eq!(
            var_server.recv().await,
            Some(Variable { name: "sis".to_string(), value: "corona".to_string() })
        );

        assert_eq!(
            var_server.recv().await,
            Some(Variable { name: "cav".to_string(), value: "babs".to_string() })
        );
        assert_eq!(
            var_server.recv().await,
            Some(Variable { name: "name".to_string(), value: "ianthe".to_string() })
        );
        assert!(var_server.recv().await.is_none());
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_get_all_vars_error() -> Result<()> {
        let (var_client, mut var_server): (Sender<Variable>, Receiver<Variable>) = mpsc::channel(2);
        let mut test_transport = TestTransport::new();
        test_transport.push(Reply::Fail("Done".to_string()));
        test_transport.push(Reply::Info("alt:kiriona".to_string()));
        let mut fastboot_client = FastbootProxy::<TestTransport> {
            target_id: "foo".to_string(),
            interface: Some(test_transport),
            interface_factory: Box::new(TestTransportFactory {}),
        };
        assert!(fastboot_client.get_all_vars(var_client).await.is_err());
        assert_eq!(
            var_server.recv().await,
            Some(Variable { name: "alt".to_string(), value: "kiriona".to_string() })
        );
        assert!(var_server.recv().await.is_none());
        Ok(())
    }

    ///////////////////////////////////////////////////////////////////////////
    // oem
    //

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_oem_ok() -> Result<()> {
        let mut test_transport = TestTransport::new();
        test_transport.push(Reply::Okay("done".to_string()));
        let mut fastboot_client = FastbootProxy::<TestTransport> {
            target_id: "foo".to_string(),
            interface: Some(test_transport),
            interface_factory: Box::new(TestTransportFactory {}),
        };

        assert_eq!(fastboot_client.target_id, "foo");
        fastboot_client.oem("version").await?;
        Ok(())
    }
    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_oem_fail() -> Result<()> {
        let mut test_transport = TestTransport::new();
        test_transport.push(Reply::Fail("this command failed".to_string()));
        let mut fastboot_client = FastbootProxy::<TestTransport> {
            target_id: "foo".to_string(),
            interface: Some(test_transport),
            interface_factory: Box::new(TestTransportFactory {}),
        };

        assert_eq!(fastboot_client.target_id, "foo");
        assert!(fastboot_client.oem("version").await.is_err());

        Ok(())
    }
    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_oem_bail_unexpected_response() -> Result<()> {
        let mut test_transport = TestTransport::new();
        test_transport.push(Reply::Data(1234));
        let mut fastboot_client = FastbootProxy::<TestTransport> {
            target_id: "foo".to_string(),
            interface: Some(test_transport),
            interface_factory: Box::new(TestTransportFactory {}),
        };

        assert_eq!(fastboot_client.target_id, "foo");
        assert!(fastboot_client.oem("version").await.is_err());

        Ok(())
    }

    ///////////////////////////////////////////////////////////////////////////
    // erase
    //

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_erase_ok() -> Result<()> {
        let mut test_transport = TestTransport::new();
        test_transport.push(Reply::Okay("done".to_string()));
        let mut fastboot_client = FastbootProxy::<TestTransport> {
            target_id: "foo".to_string(),
            interface: Some(test_transport),
            interface_factory: Box::new(TestTransportFactory {}),
        };

        assert_eq!(fastboot_client.target_id, "foo");
        fastboot_client.erase("slotA").await?;
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_erase_fail() -> Result<()> {
        let mut test_transport = TestTransport::new();
        test_transport.push(Reply::Fail("could not erase".to_string()));
        let mut fastboot_client = FastbootProxy::<TestTransport> {
            target_id: "foo".to_string(),
            interface: Some(test_transport),
            interface_factory: Box::new(TestTransportFactory {}),
        };

        assert!(fastboot_client.erase("slotB").await.is_err());

        Ok(())
    }
    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_erase_bail_unexpected_response() -> Result<()> {
        let mut test_transport = TestTransport::new();
        test_transport.push(Reply::Data(1234));
        let mut fastboot_client = FastbootProxy::<TestTransport> {
            target_id: "foo".to_string(),
            interface: Some(test_transport),
            interface_factory: Box::new(TestTransportFactory {}),
        };

        assert!(fastboot_client.erase("slotC").await.is_err());

        Ok(())
    }

    ///////////////////////////////////////////////////////////////////////////
    // boot
    //

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_boot_ok() -> Result<()> {
        let mut test_transport = TestTransport::new();
        test_transport.push(Reply::Okay("done".to_string()));
        let mut fastboot_client = FastbootProxy::<TestTransport> {
            target_id: "foo".to_string(),
            interface: Some(test_transport),
            interface_factory: Box::new(TestTransportFactory {}),
        };

        assert_eq!(fastboot_client.target_id, "foo");
        fastboot_client.boot().await?;
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_boot_fail() -> Result<()> {
        let mut test_transport = TestTransport::new();
        test_transport.push(Reply::Fail("could not boot".to_string()));
        let mut fastboot_client = FastbootProxy::<TestTransport> {
            target_id: "foo".to_string(),
            interface: Some(test_transport),
            interface_factory: Box::new(TestTransportFactory {}),
        };

        assert!(fastboot_client.boot().await.is_err());

        Ok(())
    }
    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_boot_unexpected_response() -> Result<()> {
        let mut test_transport = TestTransport::new();
        test_transport.push(Reply::Data(1234));
        let mut fastboot_client = FastbootProxy::<TestTransport> {
            target_id: "foo".to_string(),
            interface: Some(test_transport),
            interface_factory: Box::new(TestTransportFactory {}),
        };

        assert!(fastboot_client.boot().await.is_err());

        Ok(())
    }

    ///////////////////////////////////////////////////////////////////////////
    // reboot
    //

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_reboot_ok() -> Result<()> {
        let mut test_transport = TestTransport::new();
        test_transport.push(Reply::Okay("done".to_string()));
        let mut fastboot_client = FastbootProxy::<TestTransport> {
            target_id: "foo".to_string(),
            interface: Some(test_transport),
            interface_factory: Box::new(TestTransportFactory {}),
        };

        assert_eq!(fastboot_client.target_id, "foo");
        fastboot_client.reboot().await?;
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_reboot_fail() -> Result<()> {
        let mut test_transport = TestTransport::new();
        test_transport.push(Reply::Fail("could not reboot".to_string()));
        let mut fastboot_client = FastbootProxy::<TestTransport> {
            target_id: "foo".to_string(),
            interface: Some(test_transport),
            interface_factory: Box::new(TestTransportFactory {}),
        };

        assert!(fastboot_client.reboot().await.is_err());

        Ok(())
    }
    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_reboot_unexpected_response() -> Result<()> {
        let mut test_transport = TestTransport::new();
        test_transport.push(Reply::Data(1234));
        let mut fastboot_client = FastbootProxy::<TestTransport> {
            target_id: "foo".to_string(),
            interface: Some(test_transport),
            interface_factory: Box::new(TestTransportFactory {}),
        };

        assert!(fastboot_client.reboot().await.is_err());

        Ok(())
    }

    ///////////////////////////////////////////////////////////////////////////
    // reboot_bootloader
    //

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_reboot_bootloader_ok() -> Result<()> {
        let mut test_transport = TestTransport::new();
        test_transport.push(Reply::Okay("done".to_string()));
        let mut fastboot_client = FastbootProxy::<TestTransport> {
            target_id: "foo".to_string(),
            interface: Some(test_transport),
            interface_factory: Box::new(TestTransportFactory {}),
        };

        let (var_client, mut var_server): (Sender<RebootEvent>, Receiver<RebootEvent>) =
            mpsc::channel(3);

        assert_eq!(fastboot_client.target_id, "foo");
        fastboot_client.reboot_bootloader(var_client).await?;

        assert_eq!(var_server.recv().await, Some(RebootEvent::OnReboot));
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_reboot_bootloader_fail() -> Result<()> {
        let mut test_transport = TestTransport::new();
        test_transport.push(Reply::Fail("could not reboot bootloader".to_string()));
        let mut fastboot_client = FastbootProxy::<TestTransport> {
            target_id: "foo".to_string(),
            interface: Some(test_transport),
            interface_factory: Box::new(TestTransportFactory {}),
        };

        let (var_client, mut var_server): (Sender<RebootEvent>, Receiver<RebootEvent>) =
            mpsc::channel(3);
        assert!(fastboot_client.reboot_bootloader(var_client).await.is_err());

        assert!(var_server.recv().await.is_none());

        Ok(())
    }
    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_reboot_bootloader_unexpected_response() -> Result<()> {
        let mut test_transport = TestTransport::new();
        test_transport.push(Reply::Data(1234));
        let mut fastboot_client = FastbootProxy::<TestTransport> {
            target_id: "foo".to_string(),
            interface: Some(test_transport),
            interface_factory: Box::new(TestTransportFactory {}),
        };

        let (var_client, mut var_server): (Sender<RebootEvent>, Receiver<RebootEvent>) =
            mpsc::channel(3);
        assert!(fastboot_client.reboot_bootloader(var_client).await.is_err());

        assert!(var_server.recv().await.is_none());

        Ok(())
    }

    ///////////////////////////////////////////////////////////////////////////
    // continue_boot
    //

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_continue_boot_ok() -> Result<()> {
        let mut test_transport = TestTransport::new();
        test_transport.push(Reply::Okay("done".to_string()));
        let mut fastboot_client = FastbootProxy::<TestTransport> {
            target_id: "foo".to_string(),
            interface: Some(test_transport),
            interface_factory: Box::new(TestTransportFactory {}),
        };

        fastboot_client.continue_boot().await?;
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_continue_boot_fail() -> Result<()> {
        let mut test_transport = TestTransport::new();
        test_transport.push(Reply::Fail("could not continue boot".to_string()));
        let mut fastboot_client = FastbootProxy::<TestTransport> {
            target_id: "foo".to_string(),
            interface: Some(test_transport),
            interface_factory: Box::new(TestTransportFactory {}),
        };

        assert!(fastboot_client.continue_boot().await.is_err());

        Ok(())
    }
    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_continue_boot_unexpected_response() -> Result<()> {
        let mut test_transport = TestTransport::new();
        test_transport.push(Reply::Data(1234));
        let mut fastboot_client = FastbootProxy::<TestTransport> {
            target_id: "foo".to_string(),
            interface: Some(test_transport),
            interface_factory: Box::new(TestTransportFactory {}),
        };

        assert!(fastboot_client.continue_boot().await.is_err());

        Ok(())
    }

    ///////////////////////////////////////////////////////////////////////////
    // set_active
    //

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_set_active_ok() -> Result<()> {
        let mut test_transport = TestTransport::new();
        test_transport.push(Reply::Okay("done".to_string()));
        let mut fastboot_client = FastbootProxy::<TestTransport> {
            target_id: "foo".to_string(),
            interface: Some(test_transport),
            interface_factory: Box::new(TestTransportFactory {}),
        };

        assert_eq!(fastboot_client.target_id, "foo");
        fastboot_client.set_active("slotA").await?;
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_set_active_fail() -> Result<()> {
        let mut test_transport = TestTransport::new();
        test_transport.push(Reply::Fail("could not set active".to_string()));
        let mut fastboot_client = FastbootProxy::<TestTransport> {
            target_id: "foo".to_string(),
            interface: Some(test_transport),
            interface_factory: Box::new(TestTransportFactory {}),
        };

        assert!(fastboot_client.set_active("slotB").await.is_err());

        Ok(())
    }
    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_set_active_bail_unexpected_response() -> Result<()> {
        let mut test_transport = TestTransport::new();
        test_transport.push(Reply::Data(1234));
        let mut fastboot_client = FastbootProxy::<TestTransport> {
            target_id: "foo".to_string(),
            interface: Some(test_transport),
            interface_factory: Box::new(TestTransportFactory {}),
        };

        assert!(fastboot_client.set_active("slotC").await.is_err());

        Ok(())
    }

    ///////////////////////////////////////////////////////////////////////////
    // get_staged
    //

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_get_staged_ok() -> Result<()> {
        let tmpdir = TempDir::new().unwrap();

        // Generate temporary file
        let (mut file, temp_path) = NamedTempFile::new_in(&tmpdir).unwrap().into_parts();

        let mut test_transport = TestTransport::new();
        test_transport.push(Reply::Okay("Done".to_string())); // Upload Done
        test_transport.push(Reply::Data(1234)); // Upload Response
        test_transport.push(Reply::Data(12)); // Upload Response (size)

        let mut fastboot_client = FastbootProxy::<TestTransport> {
            target_id: "foo".to_string(),
            interface: Some(test_transport),
            interface_factory: Box::new(TestTransportFactory {}),
        };

        fastboot_client.get_staged(temp_path.to_str().unwrap()).await?;

        let mut buf = Vec::<u8>::new();
        file.read_to_end(&mut buf)?;
        assert_eq!(buf, [68, 65, 84, 65, 48, 48, 48, 48, 48, 52, 68, 50,]);

        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_get_staged_fail() -> Result<()> {
        let mut test_transport = TestTransport::new();
        test_transport.push(Reply::Fail("could not get staged".to_string()));
        let mut fastboot_client = FastbootProxy::<TestTransport> {
            target_id: "foo".to_string(),
            interface: Some(test_transport),
            interface_factory: Box::new(TestTransportFactory {}),
        };

        assert!(fastboot_client.get_staged("slotB").await.is_err());

        Ok(())
    }

    ///////////////////////////////////////////////////////////////////////////
    // stage
    //

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_stage_ok() -> Result<()> {
        let tmpdir = TempDir::new().unwrap();

        // Generate a large temporary file
        let (mut file, temp_path) = NamedTempFile::new_in(&tmpdir).unwrap().into_parts();
        let mut rng = SmallRng::from_entropy();
        let mut buf = Vec::<u8>::new();
        buf.resize(1 * 4096, 0);
        rng.fill_bytes(&mut buf);
        file.write_all(&buf).unwrap();
        file.flush().unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();

        let mut test_transport = TestTransport::new();
        test_transport.push(Reply::Okay("done".to_string())); // Download Okay
        test_transport.push(Reply::Data(4096)); // Download Response
        let mut fastboot_client = FastbootProxy::<TestTransport> {
            target_id: "foo".to_string(),
            interface: Some(test_transport),
            interface_factory: Box::new(TestTransportFactory {}),
        };

        let (var_client, mut var_server): (Sender<UploadProgress>, Receiver<UploadProgress>) =
            mpsc::channel(2);

        fastboot_client.stage(temp_path.to_str().unwrap(), var_client).await?;

        assert!(matches!(var_server.recv().await, Some(UploadProgress::OnStarted { size: 4096 })));
        assert!(matches!(var_server.recv().await, Some(UploadProgress::OnFinished)));
        assert!(var_server.recv().await.is_none());

        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_stage_fail() -> Result<()> {
        let tmpdir = TempDir::new().unwrap();

        // Generate a large temporary file
        let (mut file, temp_path) = NamedTempFile::new_in(&tmpdir).unwrap().into_parts();
        let mut rng = SmallRng::from_entropy();
        let mut buf = Vec::<u8>::new();
        buf.resize(1 * 4096, 0);
        rng.fill_bytes(&mut buf);
        file.write_all(&buf).unwrap();
        file.flush().unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();

        let mut test_transport = TestTransport::new();
        test_transport.push(Reply::Fail("could not stage".to_string()));

        let mut fastboot_client = FastbootProxy::<TestTransport> {
            target_id: "foo".to_string(),
            interface: Some(test_transport),
            interface_factory: Box::new(TestTransportFactory {}),
        };

        let (var_client, mut var_server): (Sender<UploadProgress>, Receiver<UploadProgress>) =
            mpsc::channel(2);

        assert!(fastboot_client.stage(temp_path.to_str().unwrap(), var_client).await.is_err());

        assert!(var_server.recv().await.is_none());
        Ok(())
    }

    ///////////////////////////////////////////////////////////////////////////
    // flash
    //

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_flash_ok() -> Result<()> {
        let _env = ffx_config::test_init().await?;

        let tmpdir = TempDir::new().unwrap();

        // Generate a large temporary file
        let (mut file, temp_path) = NamedTempFile::new_in(&tmpdir).unwrap().into_parts();
        let mut rng = SmallRng::from_entropy();
        let mut buf = Vec::<u8>::new();
        buf.resize(1 * 4096, 0);
        rng.fill_bytes(&mut buf);
        file.write_all(&buf).unwrap();
        file.flush().unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();

        let mut test_transport = TestTransport::new();
        test_transport.push(Reply::Okay("".to_string())); // Flash Ok
        test_transport.push(Reply::Okay("".to_string())); // Download Ok
        test_transport.push(Reply::Data(4096)); // Download

        let mut fastboot_client = FastbootProxy::<TestTransport> {
            target_id: "foo".to_string(),
            interface: Some(test_transport),
            interface_factory: Box::new(TestTransportFactory {}),
        };

        let (var_client, mut var_server): (Sender<UploadProgress>, Receiver<UploadProgress>) =
            mpsc::channel(100);

        fastboot_client.flash("partition1", temp_path.to_str().unwrap(), var_client).await?;

        assert!(matches!(var_server.recv().await, Some(UploadProgress::OnStarted { size: 4096 })));
        assert!(matches!(var_server.recv().await, Some(UploadProgress::OnFinished)));
        assert!(var_server.recv().await.is_none());
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_flash_fail() -> Result<()> {
        let tmpdir = TempDir::new().unwrap();

        // Generate a large temporary file
        let (mut file, temp_path) = NamedTempFile::new_in(&tmpdir).unwrap().into_parts();
        let mut rng = SmallRng::from_entropy();
        let mut buf = Vec::<u8>::new();
        buf.resize(1 * 4096, 0);
        rng.fill_bytes(&mut buf);
        file.write_all(&buf).unwrap();
        file.flush().unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();

        let mut test_transport = TestTransport::new();
        test_transport.push(Reply::Fail("could not stage".to_string()));

        let mut fastboot_client = FastbootProxy::<TestTransport> {
            target_id: "foo".to_string(),
            interface: Some(test_transport),
            interface_factory: Box::new(TestTransportFactory {}),
        };

        let (var_client, mut var_server): (Sender<UploadProgress>, Receiver<UploadProgress>) =
            mpsc::channel(2);

        assert!(fastboot_client
            .flash("partition1", temp_path.to_str().unwrap(), var_client)
            .await
            .is_err());

        assert!(var_server.recv().await.is_none());
        Ok(())
    }
}
