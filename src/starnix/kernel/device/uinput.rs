// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{fs::*, logging::*, syscalls::*, task::*, types};
use bit_vec::BitVec;
use starnix_lock::Mutex;
use std::sync::Arc;

pub fn create_uinput_device(
    _current_task: &CurrentTask,
    _id: types::DeviceType,
    _node: &FsNode,
    _flags: OpenFlags,
) -> Result<Box<dyn FileOps>, Errno> {
    Ok(Box::new(UinputDevice::new()))
}

struct UinputDeviceMutableState {
    #[allow(dead_code)]
    enabled_evbits: BitVec,
}

struct UinputDevice {
    inner: Mutex<UinputDeviceMutableState>,
}

impl UinputDevice {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(UinputDeviceMutableState {
                enabled_evbits: BitVec::from_elem(uapi::EV_CNT as usize, false),
            }),
        })
    }

    fn ui_set_evbit(&self, arg: SyscallArg) -> Result<SyscallResult, Errno> {
        let evbit: u32 = arg.into();
        match evbit {
            uapi::EV_KEY | uapi::EV_ABS => {
                self.inner.lock().enabled_evbits.set(evbit as usize, true);
                Ok(SUCCESS)
            }
            _ => {
                log_warn!("UI_SET_EVBIT with unsupported evbit {}", evbit);
                Err(types::errno!(EPERM))
            }
        }
    }
}

impl FileOps for Arc<UinputDevice> {
    fileops_impl_seekless!();
    fileops_impl_dataless!();

    fn ioctl(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        request: u32,
        arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        match request {
            uapi::UI_SET_EVBIT => self.ui_set_evbit(arg),
            _ => default_ioctl(file, current_task, request, arg),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{
        task::Kernel,
        testing::{create_kernel_and_task, AutoReleasableTask},
    };
    use test_case::test_case;

    fn make_kernel_objects(
        file: Arc<UinputDevice>,
    ) -> (Arc<Kernel>, AutoReleasableTask, FileHandle) {
        let (kernel, current_task) = create_kernel_and_task();
        let file_object = FileObject::new(
            Box::new(file),
            // The input node doesn't really live at the root of the filesystem.
            // But the test doesn't need to be 100% representative of production.
            current_task
                .lookup_path_from_root(b".")
                .expect("failed to get namespace node for root"),
            OpenFlags::empty(),
        )
        .expect("FileObject::new failed");
        (kernel, current_task, file_object)
    }

    #[test_case(uapi::EV_KEY, vec![uapi::EV_KEY as usize] => Ok(SUCCESS))]
    #[test_case(uapi::EV_ABS, vec![uapi::EV_ABS as usize] => Ok(SUCCESS))]
    #[test_case(uapi::EV_REL, vec![] => Err(types::errno!(EPERM)))]
    #[::fuchsia::test]
    async fn ui_set_evbit(bit: u32, expected_evbits: Vec<usize>) -> Result<SyscallResult, Errno> {
        let dev = UinputDevice::new();
        let (_kernel, current_task, file_object) = make_kernel_objects(dev.clone());
        let r = dev.ioctl(
            &file_object,
            &current_task,
            uapi::UI_SET_EVBIT,
            SyscallArg::from(bit as u64),
        );
        for expected_evbit in expected_evbits {
            assert!(dev.inner.lock().enabled_evbits.get(expected_evbit).unwrap());
        }
        r
    }

    #[::fuchsia::test]
    async fn ui_set_evbit_call_multi() {
        let dev = UinputDevice::new();
        let (_kernel, current_task, file_object) = make_kernel_objects(dev.clone());
        let r = dev.ioctl(
            &file_object,
            &current_task,
            uapi::UI_SET_EVBIT,
            SyscallArg::from(uapi::EV_KEY as u64),
        );
        assert_eq!(r, Ok(SUCCESS));
        let r = dev.ioctl(
            &file_object,
            &current_task,
            uapi::UI_SET_EVBIT,
            SyscallArg::from(uapi::EV_ABS as u64),
        );
        assert_eq!(r, Ok(SUCCESS));
        assert!(dev.inner.lock().enabled_evbits.get(uapi::EV_KEY as usize).unwrap());
        assert!(dev.inner.lock().enabled_evbits.get(uapi::EV_ABS as usize).unwrap());
    }
}
