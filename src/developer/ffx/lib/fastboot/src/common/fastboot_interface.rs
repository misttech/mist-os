// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Error, Result};
use async_trait::async_trait;
use tokio::sync::mpsc::Sender;

pub trait FastbootInterface: std::fmt::Debug + Fastboot {}

#[async_trait]
pub trait Fastboot {
    async fn prepare(&self, listener: Sender<RebootEvent>) -> Result<()>;

    async fn get_var(&self, name: &str) -> Result<String>;

    async fn get_all_vars(&self, listener: Sender<Variable>) -> Result<()>;

    async fn flash(
        &self,
        partition_name: &str,
        path: &str,
        listener: Sender<UploadProgress>,
    ) -> Result<()>;

    async fn erase(&self, partition_name: &str) -> Result<()>;

    async fn boot(&self) -> Result<()>;

    async fn reboot(&self) -> Result<()>;

    async fn reboot_bootloader(&self, listener: Sender<RebootEvent>) -> Result<()>;

    async fn continue_boot(&self) -> Result<()>;

    async fn get_staged(&self, path: &str) -> Result<()>;

    async fn stage(&self, path: &str, listener: Sender<UploadProgress>) -> Result<()>;

    async fn set_active(&self, slot: &str) -> Result<()>;

    async fn oem(&self, command: &str) -> Result<()>;
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
