// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    device::{
        input::{add_and_register_input_device, InputFile},
        DeviceOps,
    },
    mm::MemoryAccessorExt,
    task::CurrentTask,
    vfs::{default_ioctl, fileops_impl_seekless, FileObject, FileOps, FsNode},
};
use bit_vec::BitVec;
use fidl_fuchsia_ui_test_input::{self as futinput, RegistryRegisterKeyboardRequest};
use fuchsia_zircon as zx;
use starnix_logging::{log_info, log_trace, log_warn};
use starnix_sync::Mutex;
use starnix_syscalls::{SyscallArg, SyscallResult, SUCCESS};
use starnix_uapi::{
    device_type, error,
    errors::Errno,
    open_flags::OpenFlags,
    uapi,
    user_address::{UserAddress, UserRef},
};
use std::sync::{
    atomic::{AtomicU32, Ordering},
    Arc,
};
use zerocopy::FromBytes;

// Return the current uinput API version 5, it also told caller this uinput
// supports UI_DEV_SETUP.
const UINPUT_VERSION: u32 = 5;

#[derive(Clone)]
enum DeviceType {
    Keyboard,
    Touchscreen,
}

pub fn create_uinput_device(
    _current_task: &CurrentTask,
    _id: device_type::DeviceType,
    _node: &FsNode,
    _flags: OpenFlags,
) -> Result<Box<dyn FileOps>, Errno> {
    Ok(Box::new(UinputDevice::new()))
}

#[allow(dead_code)]
enum CreatedDevice {
    None,
    Keyboard(futinput::KeyboardSynchronousProxy),
    Touchscreen,
}

struct UinputDeviceMutableState {
    enabled_evbits: BitVec,
    input_id: Option<uapi::input_id>,
    created_device: CreatedDevice,
}

impl UinputDeviceMutableState {
    fn get_id_and_device_type(&self) -> Option<(uapi::input_id, DeviceType)> {
        let input_id = match self.input_id {
            Some(input_id) => input_id,
            None => return None,
        };
        // Currently only support Keyboard and Touchscreen, if evbits contains
        // EV_ABS, consider it is Touchscreen. This need to be revisit when we
        // want to support more device types.
        let device_type = match self.enabled_evbits.clone().get(uapi::EV_ABS as usize) {
            Some(true) => DeviceType::Touchscreen,
            Some(false) | None => DeviceType::Keyboard,
        };

        Some((input_id, device_type))
    }
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
                created_device: CreatedDevice::None,
            }),
        })
    }

    /// UI_SET_EVBIT caller pass a u32 as the event type "EV_*" to set this
    /// uinput device may handle events with the given event type.
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

    /// UI_GET_VERSION caller pass a address for u32 to `arg` to receive the
    /// uinput version. ioctl returns SUCCESS(0) for success calls, and
    /// EFAULT(14) for given address is null.
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
        match current_task.write_object(UserRef::new(user_arg), &response) {
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
            .read_object::<uapi::uinput_setup>(UserRef::new(user_arg))
            .expect("read object");
        self.inner.lock().input_id = Some(uinput_setup.id);
        Ok(SUCCESS)
    }

    fn ui_dev_create(&self, current_task: &CurrentTask) -> Result<SyscallResult, Errno> {
        // Only eng and userdebug builds include the `fuchsia.ui.test.input` service.
        let registry = match fuchsia_component::client::connect_to_protocol_sync::<
            futinput::RegistryMarker,
        >() {
            Ok(proxy) => Some(proxy),
            Err(_) => {
                log_warn!("Could not connect to fuchsia.ui.test.input/Registry");
                None
            }
        };
        self.ui_dev_create_inner(current_task, registry)
    }

    /// UI_DEV_CREATE calls create the uinput device with given information
    /// from previous ioctl() calls.
    fn ui_dev_create_inner(
        &self,
        current_task: &CurrentTask,
        // Takes `registry` arg so we can manually inject a mock registry in unit tests.
        registry: Option<futinput::RegistrySynchronousProxy>,
    ) -> Result<SyscallResult, Errno> {
        match registry {
            Some(proxy) => {
                let mut inner = self.inner.lock();
                let (input_id, device_type) = match inner.get_id_and_device_type() {
                    Some((id, dev)) => (id, dev),
                    None => return error!(EINVAL),
                };

                let server = match device_type {
                    DeviceType::Keyboard => {
                        let (key_client, key_server) =
                            fidl::endpoints::create_sync_proxy::<futinput::KeyboardMarker>();
                        inner.created_device = CreatedDevice::Keyboard(key_client);
                        key_server
                    }
                    DeviceType::Touchscreen => {
                        // TODO(b/302172833): also support touchscreen here.
                        return Ok(SUCCESS);
                    }
                };

                let device_type_clone = device_type.clone();
                match device_type_clone {
                    DeviceType::Keyboard => {
                        // Register a keyboard
                        let register_res = proxy.register_keyboard(
                            RegistryRegisterKeyboardRequest {
                                device: Some(server),
                                ..Default::default()
                            },
                            zx::Time::INFINITE,
                        );
                        if register_res.is_err() {
                            log_warn!("Uinput could not register Keyboard device to Registry");
                        }
                    }
                    DeviceType::Touchscreen => {
                        // TODO(b/302172833): also support touchscreen here.
                    }
                }

                add_and_register_input_device(
                    current_task,
                    VirtualDevice { input_id, device_type },
                );
                new_device();

                Ok(SUCCESS)
            }
            None => {
                log_warn!("No Registry available for Uinput.");
                error!(EPERM)
            }
        }
    }

    fn ui_dev_destroy(&self) -> Result<SyscallResult, Errno> {
        // TODO(b/302174354): impl ui_dev_destroy.
        log_info!("ui_dev_destroy()");

        destroy_device();

        Ok(SUCCESS)
    }
}

// TODO(b/312467059): Remove once ESC -> Power workaround can be remove.
static COUNT_OF_UINPUT_DEVICE: AtomicU32 = AtomicU32::new(0);

fn new_device() {
    let _ = COUNT_OF_UINPUT_DEVICE.fetch_add(1, Ordering::SeqCst);
}

fn destroy_device() {
    let _ = COUNT_OF_UINPUT_DEVICE.fetch_add(1, Ordering::SeqCst);
}

pub fn uinput_running() -> bool {
    COUNT_OF_UINPUT_DEVICE.load(Ordering::SeqCst) > 0
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
            uapi::UI_DEV_CREATE => self.ui_dev_create(current_task),
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
        _file: &crate::vfs::FileObject,
        _current_task: &crate::task::CurrentTask,
        _offset: usize,
        data: &mut dyn crate::vfs::buffers::InputBuffer,
    ) -> Result<usize, Errno> {
        let content = data.read_all()?;
        let event = uapi::input_event::read_from(&content)
            .expect("UInput could not create input_event from InputBuffer data.");
        log_trace!("event.type = {}", event.type_);
        log_trace!("event.code = {}", event.code);
        log_trace!("event.value = {}", event.value);

        Ok(content.len())
    }

    fn read(
        &self,
        _file: &crate::vfs::FileObject,
        _current_task: &crate::task::CurrentTask,
        _offset: usize,
        _data: &mut dyn crate::vfs::buffers::OutputBuffer,
    ) -> Result<usize, Errno> {
        log_warn!("uinput FD does not support read().");
        error!(EINVAL)
    }
}

#[derive(Clone)]
pub struct VirtualDevice {
    input_id: uapi::input_id,
    device_type: DeviceType,
}

impl DeviceOps for VirtualDevice {
    fn open(
        &self,
        current_task: &CurrentTask,
        _id: device_type::DeviceType,
        _node: &FsNode,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        match &self.device_type {
            DeviceType::Keyboard => Ok(Box::new(InputFile::new_keyboard(self.input_id))),
            DeviceType::Touchscreen => {
                let input_files_node = &current_task.kernel().input_device.input_files_node;
                // TODO(b/304595635): Check if screen size is required.
                Ok(Box::new(InputFile::new_touch(
                    self.input_id,
                    1000,
                    1000,
                    input_files_node.create_child("touch_input_file"),
                )))
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{
        mm::MemoryAccessor,
        task::Kernel,
        testing::{create_kernel_and_task, map_memory, AutoReleasableTask},
        vfs::FileHandle,
    };
    use fidl::endpoints::create_sync_proxy_and_stream;
    use fuchsia_async as fasync;
    use futures::{channel::mpsc, StreamExt};
    use starnix_uapi::user_address::UserAddress;
    use std::thread;
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
                .lookup_path_from_root(".".into())
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
        let version: u32 = current_task.read_object(version_address.into()).expect("read object");
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
        current_task.write_memory(phys_name_address, phys_name).expect("write_memory");
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
        current_task.write_object(address.into(), &setup).expect("write_memory");
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
        current_task.write_object(address.into(), &setup).expect("write_memory");
        let r = dev.ioctl(&file_object, &current_task, uapi::UI_DEV_SETUP, address.into());
        assert_eq!(r, Ok(SUCCESS));
        assert_eq!(dev.inner.lock().input_id.unwrap(), want_input_id);

        // call with invalid argument.
        let r =
            dev.ioctl(&file_object, &current_task, uapi::UI_DEV_SETUP, SyscallArg::from(0 as u64));
        assert_eq!(r, error!(EFAULT));
    }

    // TODO(b/319238817): Re-enable when the flakiness is fixed
    #[ignore]
    #[::fuchsia::test]
    async fn ui_dev_create_keyboard() {
        let dev = UinputDevice::new();
        let (_kernel, current_task, file_object) = make_kernel_objects(dev.clone());
        let address = map_memory(
            &current_task,
            UserAddress::default(),
            std::mem::size_of::<uapi::uinput_setup>() as u64,
        );
        let _ = dev.ioctl(&file_object, &current_task, uapi::UI_DEV_SETUP, address.into());

        let (req_sender, mut req_receiver) = mpsc::unbounded();
        let (registry_proxy, mut stream) =
            create_sync_proxy_and_stream::<futinput::RegistryMarker>()
                .expect("create Registry proxy and stream");

        let handle = thread::spawn(move || {
            fasync::LocalExecutor::new().run_singlethreaded(async {
                if let Some(request) = stream.next().await {
                    match request {
                        Ok(futinput::RegistryRequest::RegisterKeyboard {
                            payload,
                            responder,
                            ..
                        }) => {
                            let _ = req_sender.unbounded_send(payload.device);
                            let _ = responder.send();
                        }
                        _ => panic!("Registry handler received an unexpected request"),
                    }
                } else {
                    panic!("Registry handler did not receive RegistryRequest")
                }
            })
        });

        let res = dev.ui_dev_create_inner(&current_task, Some(registry_proxy));
        assert_eq!(res, Ok(SUCCESS));

        handle.join().expect("stream panic");

        let request = req_receiver.next().await;

        // Verify that ui_dev_create sends RegisterKeyboard request to Registry
        // and that the request includes some ServerEnd in `device`.
        assert!(request.is_some());
    }

    #[::fuchsia::test]
    async fn ui_dev_create_keyboard_fails_no_registry() {
        let dev = UinputDevice::new();
        let (_kernel, current_task, file_object) = make_kernel_objects(dev.clone());
        let address = map_memory(
            &current_task,
            UserAddress::default(),
            std::mem::size_of::<uapi::uinput_setup>() as u64,
        );
        let _ = dev.ioctl(&file_object, &current_task, uapi::UI_DEV_SETUP, address.into());
        let res = dev.ui_dev_create_inner(&current_task, None);
        assert_eq!(res, error!(EPERM))
    }
}
