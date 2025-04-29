// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_hardware_qcom_hvdcpopti as fhvdcpopti;
use starnix_core::task::CurrentTask;
use starnix_core::vfs::{BytesFile, BytesFileOps, FsNodeOps, SimpleFileNode};
use starnix_sync::Mutex;
use starnix_uapi::errors::Errno;
use starnix_uapi::{errno, error};
use std::borrow::Cow;

const HVDCP_OPTI_DIRECTORY: &str = "/svc/fuchsia.hardware.qcom.hvdcpopti.Service";
pub fn connect_to_device() -> Result<fhvdcpopti::DeviceSynchronousProxy, Errno> {
    let mut dir = std::fs::read_dir(HVDCP_OPTI_DIRECTORY).map_err(|_| errno!(EINVAL))?;
    let Some(Ok(entry)) = dir.next() else {
        return error!(EBUSY);
    };
    let path =
        entry.path().join("device").into_os_string().into_string().map_err(|_| errno!(EINVAL))?;

    let (client_channel, server_channel) = zx::Channel::create();
    fdio::service_connect(&path, server_channel).map_err(|_| errno!(EINVAL))?;

    Ok(fhvdcpopti::DeviceSynchronousProxy::new(client_channel))
}

// Current QBG context dump size is 2448 bytes (612 u32 members).
// Use greater buffer to accommodate future additions to QBG context.
const QBG_CONTEXT_LOCAL_BUF_SIZE: usize = 3072;
#[derive(Default)]
pub struct ReadWriteBytesFile {
    data: Mutex<Vec<u8>>,
}

impl ReadWriteBytesFile {
    pub fn new_node() -> impl FsNodeOps {
        SimpleFileNode::new(move || Ok(BytesFile::new(Self::default())))
    }
}

impl BytesFileOps for ReadWriteBytesFile {
    fn read(&self, _current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        Ok(self.data.lock().clone().into())
    }

    fn write(&self, _current_task: &CurrentTask, data: Vec<u8>) -> Result<(), Errno> {
        if data.len() > QBG_CONTEXT_LOCAL_BUF_SIZE {
            return error!(EINVAL);
        }

        *self.data.lock() = data;
        Ok(())
    }
}
