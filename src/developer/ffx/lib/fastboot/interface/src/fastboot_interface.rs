// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use async_trait::async_trait;
use chrono::Duration;
use tokio::sync::mpsc::Sender;

pub trait FastbootInterface: std::fmt::Debug + Fastboot {}

#[async_trait(?Send)]
pub trait Fastboot {
    async fn get_var(&mut self, name: &str) -> Result<String, FastbootError>;

    async fn get_all_vars(&mut self, listener: Sender<Variable>) -> Result<(), FastbootError>;

    async fn flash(
        &mut self,
        partition_name: &str,
        path: &str,
        listener: Sender<UploadProgress>,
        timeout: Duration,
    ) -> Result<(), FastbootError>;

    async fn erase(&mut self, partition_name: &str) -> Result<(), FastbootError>;

    async fn boot(&mut self) -> Result<(), FastbootError>;

    async fn reboot(&mut self) -> Result<(), FastbootError>;

    async fn reboot_bootloader(
        &mut self,
        listener: Sender<RebootEvent>,
    ) -> Result<(), FastbootError>;

    async fn continue_boot(&mut self) -> Result<(), FastbootError>;

    async fn get_staged(&mut self, path: &str) -> Result<(), FastbootError>;

    async fn stage(
        &mut self,
        path: &str,
        listener: Sender<UploadProgress>,
    ) -> Result<(), FastbootError>;

    async fn set_active(&mut self, slot: &str) -> Result<(), FastbootError>;

    async fn oem(&mut self, command: &str) -> Result<(), FastbootError>;
}

#[derive(Debug, PartialEq)]
pub enum RebootEvent {
    OnReboot,
}

#[derive(Debug)]
pub enum UploadProgress {
    OnStarted { size: u64 },
    OnFinished,
    OnProgress { bytes_written: u64 },
    OnError { error: anyhow::Error },
}

#[derive(Debug, PartialEq)]
pub struct Variable {
    pub name: String,
    pub value: String,
}

#[derive(thiserror::Error, Debug)]
pub enum FastbootError {
    #[error("{}", .0)]
    Error(#[from] anyhow::Error),

    #[error("Unexpected reply from fastboot device for {}: {}", method, reply)]
    UnexpectedReply { method: String, reply: String },

    #[error("Failed to set_active slot {}: {}", slot, message)]
    SetActiveFailed { slot: String, message: String },

    #[error("Failed to get {}: {}", variable, message)]
    GetVariableError { variable: String, message: String },

    #[error("Download of staged file to {} failed: {}", path, message)]
    DownloadFailed { path: String, message: String },

    #[error("Failed to oem \"{}\": {}", command, message)]
    OemCommandFailed { command: String, message: String },

    #[error("Failed to boot: {}", .0)]
    BootFailed(String),

    #[error("Reboot failed: {}", .0)]
    RebootFailed(String),

    #[error("Continuing boot failed: {}", .0)]
    ContinueBootFailed(String),

    #[error("Failed to erase partition {}: {}", partition, message)]
    ErasePartitionFailed { partition: String, message: String },

    #[error("Failed to stage file {}", .0 )]
    StageError(#[from] StageError),

    #[error("Failed to stage file {}", .0 )]
    FlashError(#[from] FlashError),

    #[error("Failed to reboot to bootloader: {}", message)]
    RebootBootloaderFailed { message: String },

    #[error("Failed to get all variables: {}", .0 )]
    GetAllVarsFailed(String),

    #[error("Did not write correct number of bytes. Got {:?}. Want {}", written, expected)]
    ShortWrite { written: usize, expected: usize },
}

#[derive(thiserror::Error, Debug)]
pub enum FlashError {
    #[error("{}", .0)]
    Error(#[from] anyhow::Error),

    #[error("{}", .0)]
    IoError(#[from] std::io::Error),

    #[error("{}", .0)]
    InvalidFileSize(#[source] std::num::TryFromIntError),

    #[error("Failed to upload {}: {}", partition, message)]
    FlashFailed { partition: String, message: String },

    #[error("{}", .0)]
    UploadFailed(#[from] StageError),

    #[error("{}", .0)]
    TimeoutError(String),
}

#[derive(thiserror::Error, Debug)]
pub enum StageError {
    #[error("{}", .0)]
    IoError(#[from] std::io::Error),

    #[error("{}", .0)]
    InvalidFileSize(#[source] std::num::TryFromIntError),

    #[error("Failed to upload {}: {}", path, message)]
    UploadFailed { path: String, message: String },
}
