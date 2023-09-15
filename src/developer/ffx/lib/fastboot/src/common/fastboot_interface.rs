// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Error, Result};
use async_trait::async_trait;
use tokio::sync::mpsc::Sender;

pub trait FastbootInterface: std::fmt::Debug + Fastboot {}

#[async_trait(?Send)]
pub trait Fastboot {
    async fn prepare(&mut self, listener: Sender<RebootEvent>) -> Result<()>;

    async fn get_var(&mut self, name: &str) -> Result<String>;

    async fn get_all_vars(&mut self, listener: Sender<Variable>) -> Result<()>;

    async fn flash(
        &mut self,
        partition_name: &str,
        path: &str,
        listener: Sender<UploadProgress>,
    ) -> Result<()>;

    async fn erase(&mut self, partition_name: &str) -> Result<()>;

    async fn boot(&mut self) -> Result<()>;

    async fn reboot(&mut self) -> Result<()>;

    async fn reboot_bootloader(&mut self, listener: Sender<RebootEvent>) -> Result<()>;

    async fn continue_boot(&mut self) -> Result<()>;

    async fn get_staged(&mut self, path: &str) -> Result<()>;

    async fn stage(&mut self, path: &str, listener: Sender<UploadProgress>) -> Result<()>;

    async fn set_active(&mut self, slot: &str) -> Result<()>;

    async fn oem(&mut self, command: &str) -> Result<()>;
}

#[derive(Debug)]
pub enum RebootEvent {
    OnReboot,
}

#[derive(Debug)]
pub enum UploadProgress {
    OnStarted { size: u64 },
    OnFinished,
    OnProgress { bytes_written: u64 },
    OnError { error: Error },
}

#[derive(Debug)]
pub struct Variable {
    pub name: String,
    pub value: String,
}
