// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    fs::{default_ioctl, fileops_impl_seekless, FileObject, FileOps, FsNode},
    logging::{log_info, log_warn},
    mm::MemoryAccessorExt,
    syscalls::{SyscallArg, SyscallResult, SUCCESS},
    task::CurrentTask,
};
use bit_vec::BitVec;
use starnix_lock::Mutex;
use starnix_uapi::{
    device_type::DeviceType,
    error,
    errors::Errno,
    open_flags::OpenFlags,
    uapi,
    user_address::{UserAddress, UserRef},
};
use std::sync::Arc;

// Return the current uinput API version 5, it also told caller this uinput
// supports UI_DEV_SETUP.
const UINPUT_VERSION: u32 = 5;

pub fn create_uinput_device(
    _current_task: &CurrentTask,
    _id: DeviceType,
    _node: &FsNode,
    _flags: OpenFlags,
) -> Result<Box<dyn FileOps>, Errno> {
    Ok(Box::new(UinputDevice::new()))
}

struct UinputDeviceMutableState {
    #[allow(dead_code)]
    enabled_evbits: BitVec,
    #[allow(dead_code)]
    input_id: Option<uapi::input_id>,
}

struct UinputDevice {
    inner: Mutex<UinputDeviceMutableState>,
}

impl UinputDevice {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(UinputDeviceMutableState {
                enabled_evbits: BitVec::from_elem(uapi::EV_CNT as usize, false),
                input_id: None,
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
                error!(EPERM)
            }
        }
    }

    // UI_GET_VERSION caller pass a address for u32 to `arg` to receive the
    // uinput version. ioctl returns SUCCESS(0) for success calls, and
    // EFAULT(14) for given address is null.
    fn ui_get_version(
        &self,
        current_task: &CurrentTask,
        arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        let user_arg = UserAddress::from(arg);
        if user_arg.is_null() {
            return error!(EFAULT);
        }
        let response: u32 = UINPUT_VERSION;
        match current_task.mm.write_object(UserRef::new(user_arg), &response) {
            Ok(_) => Ok(SUCCESS),
            Err(e) => Err(e),
        }
    }

    /// UI_DEV_SETUP set the name of device and input_id (bustype, vendor id,
    /// product id, version) to the uinput device.
    fn ui_dev_setup(
        &self,
        current_task: &CurrentTask,
        arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        let user_arg = UserAddress::from(arg);
        if user_arg.is_null() {
            return error!(EFAULT);
        }
        let uinput_setup = current_task
            .mm
            .read_object::<uapi::uinput_setup>(UserRef::new(user_arg))
            .expect("read object");
        self.inner.lock().input_id = Some(uinput_setup.id);
        Ok(SUCCESS)
    }

    fn ui_dev_create(&self) -> Result<SyscallResult, Errno> {
        // TODO(b/308156116): impl ui_dev_create.
        log_info!("ui_dev_create()");
        Ok(SUCCESS)
    }

    fn ui_dev_destroy(&self) -> Result<SyscallResult, Errno> {
        // TODO(b/302174354): impl ui_dev_destroy.
        log_info!("ui_dev_destroy()");
        Ok(SUCCESS)
    }
}

impl FileOps for Arc<UinputDevice> {
    fileops_impl_seekless!();

    fn ioctl(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        request: u32,
        arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        match request {
            uapi::UI_GET_VERSION => self.ui_get_version(current_task, arg),
            uapi::UI_SET_EVBIT => self.ui_set_evbit(arg),
            // `fuchsia.ui.test.input.Registry` does not use some uinput ioctl
            // request, just ignore the request and return SUCCESS, even args
            // is invalid.
            uapi::UI_SET_KEYBIT
            | uapi::UI_SET_ABSBIT
            | uapi::UI_SET_PHYS
            | uapi::UI_SET_PROPBIT => Ok(SUCCESS),
            uapi::UI_DEV_SETUP => self.ui_dev_setup(current_task, arg),
            uapi::UI_DEV_CREATE => self.ui_dev_create(),
            uapi::UI_DEV_DESTROY => self.ui_dev_destroy(),
            // default_ioctl() handles file system related requests and reject
            // others.
            _ => {
                log_warn!("receive unknown ioctl request: {:?}", request);
                default_ioctl(file, current_task, request, arg)
            }
        }
    }

    fn write(
        &self,
        _file: &crate::fs::FileObject,
        _current_task: &crate::task::CurrentTask,
        _offset: usize,
        _data: &mut dyn crate::fs::buffers::InputBuffer,
    ) -> Result<usize, Errno> {
        // TODO(b/308160735): impl write() for uinput FD.
        log_info!("write()");
        Ok(0)
    }

    fn read(
        &self,
        _file: &crate::fs::FileObject,
        _current_task: &crate::task::CurrentTask,
        _offset: usize,
        _data: &mut dyn crate::fs::buffers::OutputBuffer,
    ) -> Result<usize, Errno> {
        log_warn!("uinput FD does not support read().");
        error!(EINVAL)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{
        fs::FileHandle,
        mm::MemoryAccessor,
        task::Kernel,
        testing::{create_kernel_and_task, map_memory, AutoReleasableTask},
    };
    use starnix_uapi::user_address::UserAddress;
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

    #[::fuchsia::test]
    async fn ui_get_version() {
        let dev = UinputDevice::new();
        let (_kernel, current_task, file_object) = make_kernel_objects(dev.clone());
        let version_address =
            map_memory(&current_task, UserAddress::default(), std::mem::size_of::<u32>() as u64);
        let r =
            dev.ioctl(&file_object, &current_task, uapi::UI_GET_VERSION, version_address.into());
        assert_eq!(r, Ok(SUCCESS));
        let version: u32 =
            current_task.mm.read_object(version_address.into()).expect("read object");
        assert_eq!(version, UINPUT_VERSION);

        // call with invalid buffer.
        let r = dev.ioctl(
            &file_object,
            &current_task,
            uapi::UI_GET_VERSION,
            SyscallArg::from(0 as u64),
        );
        assert_eq!(r, error!(EFAULT));
    }

    #[test_case(uapi::EV_KEY, vec![uapi::EV_KEY as usize] => Ok(SUCCESS))]
    #[test_case(uapi::EV_ABS, vec![uapi::EV_ABS as usize] => Ok(SUCCESS))]
    #[test_case(uapi::EV_REL, vec![] => error!(EPERM))]
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

    #[::fuchsia::test]
    async fn ui_set_keybit() {
        let dev = UinputDevice::new();
        let (_kernel, current_task, file_object) = make_kernel_objects(dev.clone());
        let r = dev.ioctl(
            &file_object,
            &current_task,
            uapi::UI_SET_KEYBIT,
            SyscallArg::from(uapi::BTN_TOUCH as u64),
        );
        assert_eq!(r, Ok(SUCCESS));

        // also test call multi times.
        let r = dev.ioctl(
            &file_object,
            &current_task,
            uapi::UI_SET_KEYBIT,
            SyscallArg::from(uapi::KEY_SPACE as u64),
        );
        assert_eq!(r, Ok(SUCCESS));
    }

    #[::fuchsia::test]
    async fn ui_set_absbit() {
        let dev = UinputDevice::new();
        let (_kernel, current_task, file_object) = make_kernel_objects(dev.clone());
        let r = dev.ioctl(
            &file_object,
            &current_task,
            uapi::UI_SET_ABSBIT,
            SyscallArg::from(uapi::ABS_MT_SLOT as u64),
        );
        assert_eq!(r, Ok(SUCCESS));

        // also test call multi times.
        let r = dev.ioctl(
            &file_object,
            &current_task,
            uapi::UI_SET_ABSBIT,
            SyscallArg::from(uapi::ABS_MT_TOUCH_MAJOR as u64),
        );
        assert_eq!(r, Ok(SUCCESS));
    }

    #[::fuchsia::test]
    async fn ui_set_propbit() {
        let dev = UinputDevice::new();
        let (_kernel, current_task, file_object) = make_kernel_objects(dev.clone());
        let r = dev.ioctl(
            &file_object,
            &current_task,
            uapi::UI_SET_PROPBIT,
            SyscallArg::from(uapi::INPUT_PROP_DIRECT as u64),
        );
        assert_eq!(r, Ok(SUCCESS));

        // also test call multi times.
        let r = dev.ioctl(
            &file_object,
            &current_task,
            uapi::UI_SET_PROPBIT,
            SyscallArg::from(uapi::INPUT_PROP_DIRECT as u64),
        );
        assert_eq!(r, Ok(SUCCESS));
    }

    #[::fuchsia::test]
    async fn ui_set_phys() {
        let dev = UinputDevice::new();
        let (_kernel, current_task, file_object) = make_kernel_objects(dev.clone());
        let phys_name = b"mouse0\0";
        let phys_name_address =
            map_memory(&current_task, UserAddress::default(), phys_name.len() as u64);
        current_task.mm.write_memory(phys_name_address, phys_name).expect("write_memory");
        let r = dev.ioctl(&file_object, &current_task, uapi::UI_SET_PHYS, phys_name_address.into());
        assert_eq!(r, Ok(SUCCESS));

        // also test call multi times with invalid argument.
        let r =
            dev.ioctl(&file_object, &current_task, uapi::UI_SET_PHYS, SyscallArg::from(0 as u64));
        assert_eq!(r, Ok(SUCCESS));
    }

    #[::fuchsia::test]
    async fn ui_dev_setup() {
        let dev = UinputDevice::new();
        let (_kernel, current_task, file_object) = make_kernel_objects(dev.clone());
        let address = map_memory(
            &current_task,
            UserAddress::default(),
            std::mem::size_of::<uapi::uinput_setup>() as u64,
        );
        let want_input_id =
            uapi::input_id { vendor: 0x18d1, product: 0xabcd, ..uapi::input_id::default() };
        let setup = uapi::uinput_setup { id: want_input_id, ..uapi::uinput_setup::default() };
        current_task.mm.write_object(address.into(), &setup).expect("write_memory");
        let r = dev.ioctl(&file_object, &current_task, uapi::UI_DEV_SETUP, address.into());
        assert_eq!(r, Ok(SUCCESS));
        assert_eq!(dev.inner.lock().input_id.unwrap(), want_input_id);

        // call multi time updates input_id.
        let address = map_memory(
            &current_task,
            UserAddress::default(),
            std::mem::size_of::<uapi::uinput_setup>() as u64,
        );
        let want_input_id =
            uapi::input_id { vendor: 0x18d1, product: 0x1234, ..uapi::input_id::default() };
        let setup = uapi::uinput_setup { id: want_input_id, ..uapi::uinput_setup::default() };
        current_task.mm.write_object(address.into(), &setup).expect("write_memory");
        let r = dev.ioctl(&file_object, &current_task, uapi::UI_DEV_SETUP, address.into());
        assert_eq!(r, Ok(SUCCESS));
        assert_eq!(dev.inner.lock().input_id.unwrap(), want_input_id);

        // call with invalid argument.
        let r =
            dev.ioctl(&file_object, &current_task, uapi::UI_DEV_SETUP, SyscallArg::from(0 as u64));
        assert_eq!(r, error!(EFAULT));
    }
}
