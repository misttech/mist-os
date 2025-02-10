// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::device::remote_block_device::RemoteBlockDevice;
use crate::task::CurrentTask;
use anyhow::{anyhow, Error};
use bstr::{BStr, ByteSlice as _};
use starnix_logging::log_info;
use starnix_sync::{Locked, Unlocked};
use static_assertions::const_assert_eq;
use std::sync::{Arc, Weak};
use zerocopy::{FromBytes, Immutable, KnownLayout};

/// Android passes bootloader messages across reboot via a special "misc" partition.  Since Starnix
/// acts as a de-facto bootloader for Android, we need to be able to read these bootloader messages,
/// which is done through this interface.
///
/// This is a temporary hack which is only necessary because we implement Android FDR in Starnix and
/// never boot into Android's recovery mode (which normally handles the bootloader messages).  We
/// can remove this when we eventually let Android's recovery mode drive FDR.
#[derive(Debug)]
pub struct AndroidBootloaderMessageStore(Weak<RemoteBlockDevice>);

// See bootable/recovery/bootloader_message for the canonical format.
#[repr(C)]
#[derive(Copy, Clone, Immutable, KnownLayout, FromBytes)]
struct BootloaderMessageRaw {
    command: [u8; 32],
    _status: [u8; 32],
    recovery: [u8; 768],
    _stage: [u8; 32],
    _reserved: [u8; 1184],
}

fn read_null_terminated(buf: &[u8]) -> &BStr {
    if let Some((prefix, _)) = buf.split_once_str(&[0u8]) {
        prefix.as_bstr()
    } else {
        buf.as_bstr()
    }
}

#[derive(Debug, Default, Eq, PartialEq)]
pub enum BootloaderMessage {
    /// Reboot into recovery, passing the list of string flags.
    BootRecovery(Vec<String>),
    #[default]
    Unknown,
}

impl TryFrom<BootloaderMessageRaw> for BootloaderMessage {
    type Error = anyhow::Error;

    fn try_from(value: BootloaderMessageRaw) -> Result<Self, Self::Error> {
        let command = read_null_terminated(&value.command).to_str()?;
        if command.is_empty() {
            return Err(anyhow!("No command written to bootloader messages"));
        }
        match command {
            "boot-recovery" => {
                let recovery_args = read_null_terminated(&value.recovery);
                let mut args = vec![];
                for arg in recovery_args.split_str("\n") {
                    args.push(arg.to_str()?.to_owned());
                }
                Ok(BootloaderMessage::BootRecovery(args))
            }
            _ => {
                log_info!("Unrecognized bootloader command {command}");
                Ok(BootloaderMessage::Unknown)
            }
        }
    }
}

impl AndroidBootloaderMessageStore {
    pub fn new(device: &Arc<RemoteBlockDevice>) -> Self {
        Self(Arc::downgrade(device))
    }

    pub fn read_bootloader_message(&self) -> Result<BootloaderMessage, Error> {
        if let Some(device) = self.0.upgrade() {
            const_assert_eq!(std::mem::size_of::<BootloaderMessageRaw>(), 2048);
            let mut buf = vec![0u8; 2048];
            device.read(0, &mut buf)?;
            BootloaderMessageRaw::read_from_bytes(&buf[..])
                .map_err(|_| anyhow!("Failed to deserialize bootloader message"))
                .and_then(|raw| BootloaderMessage::try_from(raw))
        } else {
            Err(anyhow!("Can't read bootloader message; device is inaccessible"))
        }
    }
}

pub fn android_bootloader_message_store_init(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
) {
    let kernel = current_task.kernel().clone();
    current_task.kernel().remote_block_device_registry.on_device_added(Box::new(
        move |name, _minor, device| {
            if name == "misc" {
                log_info!(
                    "'misc' remote block device detected; setting as bootloader message store"
                );
                kernel
                    .bootloader_message_store
                    .set(AndroidBootloaderMessageStore::new(device))
                    .expect("Already initialized");
            }
        },
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use zerocopy::FromZeros as _;

    #[test]
    fn test_read_null_terminated() {
        assert_eq!(read_null_terminated(b""), b"".as_bstr());
        assert_eq!(read_null_terminated(b"\0"), b"".as_bstr());
        assert_eq!(read_null_terminated(b"foo\0bar\0"), b"foo".as_bstr());
        assert_eq!(read_null_terminated(b"foo\0\0"), b"foo".as_bstr());
    }

    #[test]
    fn test_parse_bootloader_message() {
        let mut raw = BootloaderMessageRaw::new_zeroed();
        raw.command[..14].copy_from_slice(b"boot-recovery\0");
        raw.recovery[..12].copy_from_slice(b"foo\nbar\nbaz\0");
        let message = BootloaderMessage::try_from(raw).unwrap();
        assert_eq!(
            message,
            BootloaderMessage::BootRecovery(vec![
                "foo".to_string(),
                "bar".to_string(),
                "baz".to_string()
            ])
        );

        let mut raw = BootloaderMessageRaw::new_zeroed();
        raw.command[..9].copy_from_slice(b"blahblah\0");
        let message = BootloaderMessage::try_from(raw).unwrap();
        assert_eq!(message, BootloaderMessage::Unknown);

        let raw = BootloaderMessageRaw::new_zeroed();
        BootloaderMessage::try_from(raw).unwrap_err();
    }
}
