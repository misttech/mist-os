// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use ffx_config::EnvironmentContext;
use ffx_wait_args::WaitOptions;
use ffx_writer::VerifiedMachineWriter;
use fho::{Error, FfxContext, FfxMain, FfxTool, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::time::Duration;

#[derive(Debug, Deserialize, Serialize, JsonSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CommandStatus {
    /// Successfully waited for the target (either to come up or shut down).
    Ok {},
    /// Unexpected error with string denoting error message.
    UnexpectedError { message: String },
    /// A known error that can be reported to the user.
    UserError { message: String },
}

#[cfg_attr(test, mockall::automock)]
pub trait DeviceWaiter {
    fn wait(
        &self,
        dur: Option<Duration>,
        env: &EnvironmentContext,
        target_spec: Option<String>,
        behavior: ffx_target::WaitFor,
    ) -> impl Future<Output = Result<()>>;
}

pub struct DeviceWaiterImpl;

#[async_trait(?Send)]
impl fho::TryFromEnv for DeviceWaiterImpl {
    async fn try_from_env(_env: &fho::FhoEnvironment) -> Result<Self> {
        Ok(DeviceWaiterImpl)
    }
}

impl DeviceWaiter for DeviceWaiterImpl {
    async fn wait(
        &self,
        dur: Option<Duration>,
        env: &EnvironmentContext,
        target_spec: Option<String>,
        behavior: ffx_target::WaitFor,
    ) -> Result<()> {
        ffx_target::wait_for_device(dur, env, target_spec, behavior).await
    }
}

#[derive(FfxTool)]
pub struct WaitOperation<T: DeviceWaiter + fho::TryFromEnv> {
    #[command]
    pub cmd: WaitOptions,
    pub env: EnvironmentContext,
    pub waiter: T,
}

fho::embedded_plugin!(WaitOperation<DeviceWaiterImpl>);

#[async_trait(?Send)]
impl<T: DeviceWaiter + fho::TryFromEnv> FfxMain for WaitOperation<T> {
    type Writer = VerifiedMachineWriter<CommandStatus>;

    async fn main(self, mut writer: Self::Writer) -> Result<()> {
        match self.wait_impl().await {
            Ok(()) => {
                writer.machine(&CommandStatus::Ok {})?;
                Ok(())
            }
            Err(e @ Error::User(_)) => {
                writer.machine(&CommandStatus::UserError { message: e.to_string() })?;
                Err(e)
            }
            Err(e) => {
                writer.machine(&CommandStatus::UnexpectedError { message: e.to_string() })?;
                Err(e)
            }
        }
    }
}

impl<T: DeviceWaiter + fho::TryFromEnv> WaitOperation<T> {
    pub async fn wait_impl(&self) -> Result<()> {
        let default_target: Option<String> =
            ffx_target::get_target_specifier(&self.env).await.bug()?;
        let behavior = if self.cmd.down {
            ffx_target::WaitFor::DeviceOffline
        } else {
            ffx_target::WaitFor::DeviceOnline
        };
        let duration =
            if self.cmd.timeout > 0 { Some(Duration::from_secs(self.cmd.timeout)) } else { None };
        self.waiter.wait(duration, &self.env, default_target, behavior).await
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use ffx_writer::{Format, TestBuffers};

    // This is just here to satisfy trait bounds.
    #[async_trait(?Send)]
    impl fho::TryFromEnv for MockDeviceWaiter {
        async fn try_from_env(_env: &fho::FhoEnvironment) -> Result<Self> {
            unimplemented!()
        }
    }

    #[fuchsia::test]
    async fn test_success() {
        let test_env = ffx_config::test_init().await.expect("test env");
        let mut mock_waiter = MockDeviceWaiter::new();
        mock_waiter.expect_wait().times(1).returning(|_, _, _, _| Box::pin(async { Ok(()) }));
        let tool = WaitOperation {
            cmd: WaitOptions { timeout: 1000, down: false },
            env: test_env.context.clone(),
            waiter: mock_waiter,
        };
        let test_buffers = TestBuffers::default();
        let writer = <WaitOperation<MockDeviceWaiter> as FfxMain>::Writer::new_test(
            Some(Format::JsonPretty),
            &test_buffers,
        );
        let res = tool.main(writer).await;
        let (stdout, stderr) = test_buffers.into_strings();
        assert!(res.is_ok(), "expected ok {stdout} {stderr}");
        let err = format!("schema not valid {stdout}");
        let json = serde_json::from_str(&stdout).expect(&err);
        let err = format!("json must adhere to schema: {json}");
        <WaitOperation<MockDeviceWaiter> as FfxMain>::Writer::verify_schema(&json).expect(&err)
    }

    #[fuchsia::test]
    async fn test_unexpected_error() {
        let test_env = ffx_config::test_init().await.expect("test env");
        let mut mock_waiter = MockDeviceWaiter::new();
        mock_waiter
            .expect_wait()
            .times(1)
            .returning(|_, _, _, _| Box::pin(async { Err(fho::bug!("oh no!")) }));
        let tool = WaitOperation {
            cmd: WaitOptions { timeout: 1000, down: false },
            env: test_env.context.clone(),
            waiter: mock_waiter,
        };
        let test_buffers = TestBuffers::default();
        let writer = <WaitOperation<MockDeviceWaiter> as FfxMain>::Writer::new_test(
            Some(Format::JsonPretty),
            &test_buffers,
        );
        let res = tool.main(writer).await;
        let (stdout, stderr) = test_buffers.into_strings();
        assert!(res.is_err(), "expected error {stdout} {stderr}");
        assert!(
            matches!(res, Err(Error::Unexpected(_))),
            "expected 'unexpected error' {stdout} {stderr}"
        );
        let err = format!("schema not valid {stdout}");
        let json = serde_json::from_str(&stdout).expect(&err);
        let err = format!("json must adhere to schema: {json}");
        <WaitOperation<MockDeviceWaiter> as FfxMain>::Writer::verify_schema(&json).expect(&err)
    }

    #[fuchsia::test]
    async fn test_user_error() {
        let test_env = ffx_config::test_init().await.expect("test env");
        let mut mock_waiter = MockDeviceWaiter::new();
        mock_waiter
            .expect_wait()
            .times(1)
            .returning(|_, _, _, _| Box::pin(async { Err(fho::user_error!("oh no!")) }));
        let tool = WaitOperation {
            cmd: WaitOptions { timeout: 1000, down: false },
            env: test_env.context.clone(),
            waiter: mock_waiter,
        };
        let test_buffers = TestBuffers::default();
        let writer = <WaitOperation<MockDeviceWaiter> as FfxMain>::Writer::new_test(
            Some(Format::JsonPretty),
            &test_buffers,
        );
        let res = tool.main(writer).await;
        let (stdout, stderr) = test_buffers.into_strings();
        assert!(res.is_err(), "expected error {stdout} {stderr}");
        assert!(matches!(res, Err(Error::User(_))), "expected 'user error' {stdout} {stderr}");
        let err = format!("schema not valid {stdout}");
        let json = serde_json::from_str(&stdout).expect(&err);
        let err = format!("json must adhere to schema: {json}");
        <WaitOperation<MockDeviceWaiter> as FfxMain>::Writer::verify_schema(&json).expect(&err)
    }
}
