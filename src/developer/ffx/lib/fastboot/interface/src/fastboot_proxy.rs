// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::fastboot_interface::{
    Fastboot, FastbootError, FastbootInterface, FlashError, RebootEvent, StageError,
    UploadProgress, Variable,
};
use crate::interface_factory::InterfaceFactory;
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use chrono::Duration;
use fastboot::command::{ClientVariable, Command};
use fastboot::reply::Reply;
use fastboot::{
    download, send, send_with_listener, send_with_timeout, upload, upload_with_read_timeout,
    SendError, UploadError,
};
use futures::io::{AsyncRead, AsyncWrite};
use std::fmt::Debug;
use std::fs::File;
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
// const FLASH_TIMEOUT_RATE: &str = "fastboot.flash.timeout_rate";
/// The minimum flash timeout (in seconds) for flashing to a target device
// const MIN_FLASH_TIMEOUT: &str = "fastboot.flash.min_timeout_secs";

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
    async fn on_error(&self, error: &UploadError) -> Result<()> {
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

        //Reconnect
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
    #[tracing::instrument]
    async fn get_var(&mut self, name: &str) -> core::result::Result<String, FastbootError> {
        let command = Command::GetVar(ClientVariable::Oem(name.to_string()));
        match send(command.clone(), self.interface().await?).await {
            Ok(r) => match r {
                Reply::Okay(v) => Ok(v),
                Reply::Fail(message) => {
                    Err(FastbootError::GetVariableError { variable: name.to_string(), message })
                }
                r @ _ => Err(FastbootError::UnexpectedReply {
                    method: command.to_string(),
                    reply: r.to_string(),
                }),
            },
            Err(e) => Err(FastbootError::Error(e)),
        }
    }

    #[tracing::instrument]
    async fn get_all_vars(&mut self, listener: Sender<Variable>) -> Result<(), FastbootError> {
        let variable_listener = VariableListener::new(listener)?;
        let command = Command::GetVar(ClientVariable::All);
        match send_with_listener(command.clone(), self.interface().await?, &variable_listener)
            .await?
        {
            Reply::Okay(_) => Ok(()),
            Reply::Fail(s) => Err(FastbootError::GetAllVarsFailed(s)),
            r @ _ => Err(FastbootError::UnexpectedReply {
                method: command.to_string(),
                reply: r.to_string(),
            }),
        }
    }

    #[tracing::instrument]
    async fn flash(
        &mut self,
        partition_name: &str,
        path: &str,
        listener: Sender<UploadProgress>,
        timeout: Duration,
    ) -> Result<(), FastbootError> {
        // TODO(colnnelson): This file size could be done better.
        // The stage function could return back how many bytes were uploaded
        //

        // Upload file
        let mut file_to_flash = File::open(path).map_err(FlashError::from)?;
        let size = file_to_flash.metadata().map_err(FlashError::from)?.len();
        let size = u32::try_from(size).map_err(|e| FlashError::InvalidFileSize(e))?;
        let progress_listener = ProgressListener::new(listener)?;
        let span = tracing::span!(tracing::Level::INFO, "device_flash_upload").entered();
        let upload_reply = upload_with_read_timeout(
            size,
            &mut file_to_flash,
            self.interface().await?,
            &progress_listener,
            timeout,
        )
        .await
        .context(format!("uploading {}", path))?;
        drop(span);
        match upload_reply {
            Reply::Okay(s) => tracing::debug!("Received response from download command: {}", s),
            Reply::Fail(s) => {
                return Err(FastbootError::StageError(StageError::UploadFailed {
                    path: path.to_string(),
                    message: s,
                }))
            }
            r @ _ => {
                return Err(FastbootError::UnexpectedReply {
                    method: Command::Download(size).to_string(),
                    reply: r.to_string(),
                })
            }
        };

        // Flash the uploaded file
        let span = tracing::span!(tracing::Level::INFO, "device_flash").entered();
        let command = Command::Flash(partition_name.to_string());
        let send_reply = send_with_timeout(command.clone(), self.interface().await?, timeout)
            .await
            .context("sending flash");
        drop(span);
        match send_reply {
            Ok(reply) => match reply {
                Reply::Okay(_) => Ok(()),
                Reply::Fail(s) => Err(FastbootError::FlashError(FlashError::FlashFailed {
                    partition: partition_name.to_string(),
                    message: s,
                })),
                r @ _ => Err(FastbootError::UnexpectedReply {
                    method: command.to_string(),
                    reply: r.to_string(),
                }),
            },
            Err(ref e) => {
                if let Some(ffx_err) = e.downcast_ref::<SendError>() {
                    match ffx_err {
                        SendError::Timeout => {
                            let message = format!(
                                "Time out while waiting on a response from the device. \n\
                                    The current timeout is {}.  Try increacing the timeout",
                                timeout
                            );
                            Err(FastbootError::FlashError(FlashError::TimeoutError(message)))
                        }
                    }
                } else {
                    Err(FastbootError::FlashError(
                        send_reply.map_err(FlashError::from).err().unwrap(),
                    ))
                }
            }
        }
    }

    #[tracing::instrument]
    async fn erase(&mut self, partition_name: &str) -> Result<(), FastbootError> {
        let command = Command::Erase(partition_name.to_string());
        let reply =
            send(command.clone(), self.interface().await?).await.context("sending erase")?;
        match reply {
            Reply::Okay(_) => {
                tracing::debug!("Successfully erased parition: {}", partition_name);
                Ok(())
            }
            Reply::Fail(s) => {
                return Err(FastbootError::ErasePartitionFailed {
                    partition: partition_name.to_string(),
                    message: s.to_string(),
                })
            }
            r @ _ => Err(FastbootError::UnexpectedReply {
                method: command.to_string(),
                reply: r.to_string(),
            }),
        }
    }

    #[tracing::instrument]
    async fn boot(&mut self) -> Result<(), FastbootError> {
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

            Reply::Fail(s) => return Err(FastbootError::BootFailed(s)),
            r @ _ => Err(FastbootError::UnexpectedReply {
                method: Command::Reboot.to_string(),
                reply: r.to_string(),
            }),
        }
    }

    #[tracing::instrument]
    async fn reboot(&mut self) -> Result<(), FastbootError> {
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
            Reply::Fail(s) => return Err(FastbootError::RebootFailed(s)),
            r @ _ => Err(FastbootError::UnexpectedReply {
                method: Command::Reboot.to_string(),
                reply: r.to_string(),
            }),
        }
    }

    #[tracing::instrument]
    async fn reboot_bootloader(
        &mut self,
        listener: Sender<RebootEvent>,
    ) -> Result<(), FastbootError> {
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
            Reply::Fail(s) => return Err(FastbootError::RebootBootloaderFailed { message: s }),
            r @ _ => {
                return Err(FastbootError::UnexpectedReply {
                    method: Command::RebootBootLoader.to_string(),
                    reply: r.to_string(),
                })
            }
        };
        // Once the target is rebooted, reconnect
        self.reconnect().await.context("reconnecting after rebooting to bootloader")?;
        Ok(())
    }

    #[tracing::instrument]
    async fn continue_boot(&mut self) -> Result<(), FastbootError> {
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
            Reply::Fail(s) => return Err(FastbootError::ContinueBootFailed(s)),
            r @ _ => Err(FastbootError::UnexpectedReply {
                method: Command::Continue.to_string(),
                reply: r.to_string(),
            }),
        }
    }

    #[tracing::instrument]
    async fn get_staged(&mut self, path: &str) -> Result<(), FastbootError> {
        match download(&path.to_string(), self.interface().await?)
            .await
            .context(format!("downloading to {}", path))?
        {
            Reply::Okay(_) => {
                tracing::debug!("Successfully downloaded to \"{}\"", path);
                Ok(())
            }
            Reply::Fail(message) => {
                return Err(FastbootError::DownloadFailed { path: path.to_string(), message })
            }
            r @ _ => Err(FastbootError::UnexpectedReply {
                method: Command::Upload.to_string(),
                reply: r.to_string(),
            }),
        }
    }

    #[tracing::instrument]
    async fn stage(
        &mut self,
        path: &str,
        listener: Sender<UploadProgress>,
    ) -> Result<(), FastbootError> {
        let progress_listener = ProgressListener::new(listener)?;
        let mut file_to_stage = File::open(path).map_err(StageError::from)?;
        let size = file_to_stage.metadata().map_err(StageError::from)?.len();
        let size = u32::try_from(size).map_err(|e| StageError::InvalidFileSize(e))?;
        tracing::debug!("uploading file size: {}", size);
        match upload(size, &mut file_to_stage, self.interface().await?, &progress_listener)
            .await
            .context(format!("uploading {}", path))?
        {
            Reply::Okay(s) => {
                tracing::debug!("Received response from download command: {}", s);
                Ok(())
            }
            Reply::Fail(s) => {
                return Err(FastbootError::StageError(StageError::UploadFailed {
                    path: path.to_string(),
                    message: s,
                }))
            }
            r @ _ => Err(FastbootError::UnexpectedReply {
                method: Command::Download(size).to_string(),
                reply: r.to_string(),
            }),
        }
    }

    #[tracing::instrument]
    async fn set_active(&mut self, slot: &str) -> Result<(), FastbootError> {
        let command = Command::SetActive(slot.to_string());
        match send(command.clone(), self.interface().await?).await.context("sending set_active")? {
            Reply::Okay(_) => {
                tracing::debug!("Successfully sent set_active");
                Ok(())
            }
            Reply::Fail(message) => {
                return Err(FastbootError::SetActiveFailed { slot: slot.to_string(), message })
            }
            r @ _ => Err(FastbootError::UnexpectedReply {
                method: command.to_string(),
                reply: r.to_string(),
            }),
        }
    }

    #[tracing::instrument]
    async fn oem(&mut self, command: &str) -> Result<(), FastbootError> {
        let command = Command::Oem(command.to_string());
        match send(command.clone(), self.interface().await?).await.context("sending oem")? {
            Reply::Okay(_) => {
                tracing::debug!("Successfully sent oem command \"{}\"", command);
                Ok(())
            }
            Reply::Fail(message) => {
                return Err(FastbootError::OemCommandFailed {
                    command: command.to_string(),
                    message,
                })
            }
            r @ _ => Err(FastbootError::UnexpectedReply {
                method: command.to_string(),
                reply: r.to_string(),
            }),
        }
    }
}

////////////////////////////////////////////////////////////////////////////////
// tests

#[cfg(test)]
mod test {
    use super::*;
    use crate::interface_factory::{InterfaceFactoryBase, InterfaceFactoryError};
    use fastboot::test_transport::TestTransport;
    use pretty_assertions::assert_eq;
    use rand::rngs::SmallRng;
    use rand::{RngCore, SeedableRng};
    use std::io::{Read, Seek, SeekFrom, Write};
    use tempfile::{NamedTempFile, TempDir};
    use tokio::sync::mpsc;
    use tokio::sync::mpsc::Receiver;

    #[derive(Default, Debug, Clone)]
    struct TestTransportFactory {}

    #[async_trait(?Send)]
    impl InterfaceFactoryBase<TestTransport> for TestTransportFactory {
        async fn open(&mut self) -> Result<TestTransport, InterfaceFactoryError> {
            Ok(TestTransport::new())
        }

        async fn close(&self) {}

        async fn rediscover(&mut self) -> Result<(), InterfaceFactoryError> {
            Ok(())
        }
    }

    impl InterfaceFactory<TestTransport> for TestTransportFactory {}

    ///////////////////////////////////////////////////////////////////////////
    //  get_var
    //

    #[fuchsia::test]
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

    #[fuchsia::test]
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

    #[fuchsia::test]
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

    #[fuchsia::test]
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
    #[fuchsia::test]
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
    #[fuchsia::test]
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

    #[fuchsia::test]
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

    #[fuchsia::test]
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
    #[fuchsia::test]
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

    #[fuchsia::test]
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

    #[fuchsia::test]
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

    #[fuchsia::test]
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

    #[fuchsia::test]
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

    #[fuchsia::test]
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
    #[fuchsia::test]
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

    #[fuchsia::test]
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

    #[fuchsia::test]
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
    #[fuchsia::test]
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

    #[fuchsia::test]
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

    #[fuchsia::test]
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
    #[fuchsia::test]
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

    #[fuchsia::test]
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

    #[fuchsia::test]
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
    #[fuchsia::test]
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

    #[fuchsia::test]
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

    #[fuchsia::test]
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

    #[fuchsia::test]
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
            mpsc::channel(3);

        fastboot_client.stage(temp_path.to_str().unwrap(), var_client).await?;

        assert!(matches!(var_server.recv().await, Some(UploadProgress::OnStarted { size: 4096 })));
        assert!(matches!(
            var_server.recv().await,
            Some(UploadProgress::OnProgress { bytes_written: 4096 })
        ));
        assert!(matches!(var_server.recv().await, Some(UploadProgress::OnFinished)));
        assert!(var_server.recv().await.is_none());

        Ok(())
    }

    #[fuchsia::test]
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

    #[fuchsia::test]
    async fn test_flash_ok() -> Result<()> {
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
            mpsc::channel(3);

        fastboot_client
            .flash("partition1", temp_path.to_str().unwrap(), var_client, Duration::seconds(1))
            .await?;

        assert!(matches!(var_server.recv().await, Some(UploadProgress::OnStarted { size: 4096 })));
        assert!(matches!(
            var_server.recv().await,
            Some(UploadProgress::OnProgress { bytes_written: 4096 })
        ));
        assert!(matches!(var_server.recv().await, Some(UploadProgress::OnFinished)));
        assert!(var_server.recv().await.is_none());
        Ok(())
    }

    #[fuchsia::test]
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
            .flash("partition1", temp_path.to_str().unwrap(), var_client, Duration::seconds(1))
            .await
            .is_err());

        assert!(var_server.recv().await.is_none());
        Ok(())
    }
}
