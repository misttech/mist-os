// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    device::{
        input::{add_and_register_input_device, InputFile},
        input_event_conversion::{LinuxKeyboardEventParser, LinuxTouchEventParser},
        kobject::Device,
        DeviceOps,
    },
    mm::MemoryAccessorExt,
    task::CurrentTask,
    vfs::{self, default_ioctl, fileops_impl_seekless, FileObject, FileOps, FsNode},
};
use bit_vec::BitVec;
use fidl_fuchsia_ui_test_input::{
    self as futinput, KeyboardSimulateKeyEventRequest, RegistryRegisterKeyboardRequest,
    RegistryRegisterTouchScreenRequest,
};
use fuchsia_zircon as zx;
use starnix_logging::log_warn;
use starnix_sync::{DeviceOpen, FileOpsCore, LockBefore, Locked, Mutex, Unlocked, WriteOps};
use starnix_syscalls::{SyscallArg, SyscallResult, SUCCESS};
use starnix_uapi::{
    device_type, errno, error, errors::Errno, open_flags::OpenFlags, uapi, user_address::UserRef,
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
    _locked: &mut Locked<'_, DeviceOpen>,
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
    Keyboard(futinput::KeyboardSynchronousProxy, LinuxKeyboardEventParser),
    Touchscreen(futinput::TouchScreenSynchronousProxy, LinuxTouchEventParser),
}

struct UinputDeviceMutableState {
    enabled_evbits: BitVec,
    input_id: Option<uapi::input_id>,
    created_device: CreatedDevice,
    k_device: Option<Device>,
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
                k_device: None,
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
        user_version: UserRef<u32>,
    ) -> Result<SyscallResult, Errno> {
        let response: u32 = UINPUT_VERSION;
        current_task.write_object(user_version, &response)?;
        Ok(SUCCESS)
    }

    /// UI_DEV_SETUP set the name of device and input_id (bustype, vendor id,
    /// product id, version) to the uinput device.
    fn ui_dev_setup(
        &self,
        current_task: &CurrentTask,
        user_uinput_setup: UserRef<uapi::uinput_setup>,
    ) -> Result<SyscallResult, Errno> {
        let uinput_setup = current_task.read_object(user_uinput_setup)?;
        self.inner.lock().input_id = Some(uinput_setup.id);
        Ok(SUCCESS)
    }

    fn ui_dev_create<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
    ) -> Result<SyscallResult, Errno>
    where
        L: LockBefore<FileOpsCore>,
    {
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
        self.ui_dev_create_inner(locked, current_task, registry)
    }

    /// UI_DEV_CREATE calls create the uinput device with given information
    /// from previous ioctl() calls.
    fn ui_dev_create_inner<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        // Takes `registry` arg so we can manually inject a mock registry in unit tests.
        registry: Option<futinput::RegistrySynchronousProxy>,
    ) -> Result<SyscallResult, Errno>
    where
        L: LockBefore<FileOpsCore>,
    {
        match registry {
            Some(proxy) => {
                let mut inner = self.inner.lock();
                let (input_id, device_type) = match inner.get_id_and_device_type() {
                    Some((id, dev)) => (id, dev),
                    None => return error!(EINVAL),
                };

                match device_type {
                    DeviceType::Keyboard => {
                        let (key_client, key_server) =
                            fidl::endpoints::create_sync_proxy::<futinput::KeyboardMarker>();
                        inner.created_device =
                            CreatedDevice::Keyboard(key_client, LinuxKeyboardEventParser::create());

                        // Register a keyboard
                        let register_res = proxy.register_keyboard(
                            RegistryRegisterKeyboardRequest {
                                device: Some(key_server),
                                ..Default::default()
                            },
                            zx::Time::INFINITE,
                        );
                        if register_res.is_err() {
                            log_warn!("Uinput could not register Keyboard device to Registry");
                        }
                    }
                    DeviceType::Touchscreen => {
                        let (touch_client, touch_server) =
                            fidl::endpoints::create_sync_proxy::<futinput::TouchScreenMarker>();
                        inner.created_device = CreatedDevice::Touchscreen(
                            touch_client,
                            LinuxTouchEventParser::create(),
                        );

                        // Register a touchscreen
                        let register_res = proxy.register_touch_screen(
                            RegistryRegisterTouchScreenRequest {
                                device: Some(touch_server),
                                ..Default::default()
                            },
                            zx::Time::INFINITE,
                        );
                        if register_res.is_err() {
                            log_warn!("Uinput could not register Keyboard device to Registry");
                        }
                    }
                };

                let device = add_and_register_input_device(
                    locked,
                    current_task,
                    VirtualDevice { input_id, device_type },
                );
                inner.k_device = Some(device);

                new_device();

                Ok(SUCCESS)
            }
            None => {
                log_warn!("No Registry available for Uinput.");
                error!(EPERM)
            }
        }
    }

    fn ui_dev_destroy<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
    ) -> Result<SyscallResult, Errno>
    where
        L: LockBefore<FileOpsCore>,
    {
        let mut inner = self.inner.lock();
        match inner.k_device.clone() {
            Some(device) => {
                let kernel = current_task.kernel();
                kernel.device_registry.remove_device(locked, current_task, device);
            }
            None => {
                log_warn!("UI_DEV_DESTROY kHandle not found");
                return error!(EPERM);
            }
        }
        inner.k_device = None;
        inner.created_device = CreatedDevice::None;

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
        locked: &mut Locked<'_, Unlocked>,
        file: &FileObject,
        current_task: &CurrentTask,
        request: u32,
        arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        match request {
            uapi::UI_GET_VERSION => self.ui_get_version(current_task, arg.into()),
            uapi::UI_SET_EVBIT => self.ui_set_evbit(arg),
            // `fuchsia.ui.test.input.Registry` does not use some uinput ioctl
            // request, just ignore the request and return SUCCESS, even args
            // is invalid.
            uapi::UI_SET_KEYBIT
            | uapi::UI_SET_ABSBIT
            | uapi::UI_SET_PHYS
            | uapi::UI_SET_PROPBIT => Ok(SUCCESS),
            uapi::UI_DEV_SETUP => self.ui_dev_setup(current_task, arg.into()),
            uapi::UI_DEV_CREATE => self.ui_dev_create(locked, current_task),
            uapi::UI_DEV_DESTROY => self.ui_dev_destroy(locked, current_task),
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
        _locked: &mut Locked<'_, WriteOps>,
        _file: &vfs::FileObject,
        _current_task: &crate::task::CurrentTask,
        _offset: usize,
        data: &mut dyn vfs::buffers::InputBuffer,
    ) -> Result<usize, Errno> {
        let content = data.read_all()?;
        let event = uapi::input_event::read_from(&content).ok_or_else(|| errno!(EINVAL))?;

        let mut inner = self.inner.lock();

        match &mut inner.created_device {
            CreatedDevice::Keyboard(proxy, parser) => {
                let input_report = parser.handle(event);
                match input_report {
                    Ok(Some(report)) => {
                        if let Some(keyboard_report) = report.keyboard {
                            let res = proxy.simulate_key_event(
                                &KeyboardSimulateKeyEventRequest {
                                    report: Some(keyboard_report),
                                    ..Default::default()
                                },
                                zx::Time::INFINITE,
                            );
                            if res.is_err() {
                                return error!(EIO);
                            }
                        }
                    }
                    Ok(None) => (),
                    Err(e) => return Err(e),
                }
            }
            CreatedDevice::Touchscreen(proxy, parser) => {
                let input_report = parser.handle(event);
                match input_report {
                    Ok(Some(report)) => {
                        if let Some(touch_report) = report.touch {
                            let res = proxy.simulate_touch_event(&touch_report, zx::Time::INFINITE);
                            if res.is_err() {
                                return error!(EIO);
                            }
                        }
                    }
                    Ok(None) => (),
                    Err(e) => return Err(e),
                }
            }
            CreatedDevice::None => return error!(EINVAL),
        }

        Ok(content.len())
    }

    fn read(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &vfs::FileObject,
        _current_task: &crate::task::CurrentTask,
        _offset: usize,
        _data: &mut dyn vfs::buffers::OutputBuffer,
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
        _locked: &mut Locked<'_, DeviceOpen>,
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
        testing::{create_kernel_task_and_unlocked, map_memory, AutoReleasableTask},
        vfs::{FileHandle, VecInputBuffer},
    };
    use fidl::endpoints::create_sync_proxy_and_stream;
    use fidl_fuchsia_input::Key;
    use fidl_fuchsia_input_report as fir;
    use fuchsia_async as fasync;
    use futures::StreamExt;
    use starnix_uapi::user_address::UserAddress;
    use std::thread;
    use test_case::test_case;
    use zerocopy::AsBytes;

    fn make_kernel_objects<'l>(
        file: Arc<UinputDevice>,
    ) -> (Arc<Kernel>, AutoReleasableTask, FileHandle, Locked<'l, Unlocked>) {
        let (kernel, current_task, locked) = create_kernel_task_and_unlocked();
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
        (kernel, current_task, file_object, locked)
    }

    fn make_write_input_buffer(ty: u32, code: u32, value: i32) -> VecInputBuffer {
        let buffer_data: VecInputBuffer =
            uapi::input_event { type_: ty as u16, code: code as u16, value, ..Default::default() }
                .as_bytes()
                .to_vec()
                .into();

        buffer_data
    }

    fn assert_write(
        dev: &Arc<UinputDevice>,
        locked_write: &mut Locked<'_, WriteOps>,
        file_object: &vfs::FileObject,
        current_task: &crate::task::CurrentTask,
        offset: usize,
        data: &mut dyn vfs::buffers::InputBuffer,
    ) -> () {
        let expected_res = data.available();
        let res = dev.write(locked_write, &file_object, &current_task, offset, data);
        assert_eq!(res, Ok(expected_res));
    }

    #[::fuchsia::test]
    async fn ui_get_version() {
        let dev = UinputDevice::new();
        let (_kernel, current_task, file_object, mut locked) = make_kernel_objects(dev.clone());
        let version_address =
            map_memory(&current_task, UserAddress::default(), std::mem::size_of::<u32>() as u64);
        let r = dev.ioctl(
            &mut locked,
            &file_object,
            &current_task,
            uapi::UI_GET_VERSION,
            version_address.into(),
        );
        assert_eq!(r, Ok(SUCCESS));
        let version: u32 = current_task.read_object(version_address.into()).expect("read object");
        assert_eq!(version, UINPUT_VERSION);

        // call with invalid buffer.
        let r = dev.ioctl(
            &mut locked,
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
        let (_kernel, current_task, file_object, mut locked) = make_kernel_objects(dev.clone());
        let r = dev.ioctl(
            &mut locked,
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
        let (_kernel, current_task, file_object, mut locked) = make_kernel_objects(dev.clone());
        let r = dev.ioctl(
            &mut locked,
            &file_object,
            &current_task,
            uapi::UI_SET_EVBIT,
            SyscallArg::from(uapi::EV_KEY as u64),
        );
        assert_eq!(r, Ok(SUCCESS));
        let r = dev.ioctl(
            &mut locked,
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
        let (_kernel, current_task, file_object, mut locked) = make_kernel_objects(dev.clone());
        let r = dev.ioctl(
            &mut locked,
            &file_object,
            &current_task,
            uapi::UI_SET_KEYBIT,
            SyscallArg::from(uapi::BTN_TOUCH as u64),
        );
        assert_eq!(r, Ok(SUCCESS));

        // also test call multi times.
        let r = dev.ioctl(
            &mut locked,
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
        let (_kernel, current_task, file_object, mut locked) = make_kernel_objects(dev.clone());
        let r = dev.ioctl(
            &mut locked,
            &file_object,
            &current_task,
            uapi::UI_SET_ABSBIT,
            SyscallArg::from(uapi::ABS_MT_SLOT as u64),
        );
        assert_eq!(r, Ok(SUCCESS));

        // also test call multi times.
        let r = dev.ioctl(
            &mut locked,
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
        let (_kernel, current_task, file_object, mut locked) = make_kernel_objects(dev.clone());
        let r = dev.ioctl(
            &mut locked,
            &file_object,
            &current_task,
            uapi::UI_SET_PROPBIT,
            SyscallArg::from(uapi::INPUT_PROP_DIRECT as u64),
        );
        assert_eq!(r, Ok(SUCCESS));

        // also test call multi times.
        let r = dev.ioctl(
            &mut locked,
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
        let (_kernel, current_task, file_object, mut locked) = make_kernel_objects(dev.clone());
        let phys_name = b"mouse0\0";
        let phys_name_address =
            map_memory(&current_task, UserAddress::default(), phys_name.len() as u64);
        current_task.write_memory(phys_name_address, phys_name).expect("write_memory");
        let r = dev.ioctl(
            &mut locked,
            &file_object,
            &current_task,
            uapi::UI_SET_PHYS,
            phys_name_address.into(),
        );
        assert_eq!(r, Ok(SUCCESS));

        // also test call multi times with invalid argument.
        let r = dev.ioctl(
            &mut locked,
            &file_object,
            &current_task,
            uapi::UI_SET_PHYS,
            SyscallArg::from(0 as u64),
        );
        assert_eq!(r, Ok(SUCCESS));
    }

    #[::fuchsia::test]
    async fn ui_dev_setup() {
        let dev = UinputDevice::new();
        let (_kernel, current_task, file_object, mut locked) = make_kernel_objects(dev.clone());
        let address = map_memory(
            &current_task,
            UserAddress::default(),
            std::mem::size_of::<uapi::uinput_setup>() as u64,
        );
        let want_input_id =
            uapi::input_id { vendor: 0x18d1, product: 0xabcd, ..uapi::input_id::default() };
        let setup = uapi::uinput_setup { id: want_input_id, ..uapi::uinput_setup::default() };
        current_task.write_object(address.into(), &setup).expect("write_memory");
        let r =
            dev.ioctl(&mut locked, &file_object, &current_task, uapi::UI_DEV_SETUP, address.into());
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
        let r =
            dev.ioctl(&mut locked, &file_object, &current_task, uapi::UI_DEV_SETUP, address.into());
        assert_eq!(r, Ok(SUCCESS));
        assert_eq!(dev.inner.lock().input_id.unwrap(), want_input_id);

        // call with invalid argument.
        let r = dev.ioctl(
            &mut locked,
            &file_object,
            &current_task,
            uapi::UI_DEV_SETUP,
            SyscallArg::from(0 as u64),
        );
        assert_eq!(r, error!(EFAULT));
    }

    #[fasync::run(2, test)]
    async fn ui_dev_create_keyboard() {
        let dev = UinputDevice::new();
        let (_kernel, current_task, file_object, mut locked) = make_kernel_objects(dev.clone());
        let address = map_memory(
            &current_task,
            UserAddress::default(),
            std::mem::size_of::<uapi::uinput_setup>() as u64,
        );
        let _ = dev.ioctl(
            &mut locked,
            &file_object,
            &current_task,
            uapi::UI_SET_EVBIT,
            SyscallArg::from(uapi::EV_KEY as u64),
        );
        let _ =
            dev.ioctl(&mut locked, &file_object, &current_task, uapi::UI_DEV_SETUP, address.into());

        let (registry_proxy, mut stream) =
            create_sync_proxy_and_stream::<futinput::RegistryMarker>()
                .expect("create Registry proxy and stream");

        let handle = thread::spawn(move || {
            fasync::LocalExecutor::new().run_singlethreaded(async {
                if let Some(request) = stream.next().await {
                    match request {
                        Ok(futinput::RegistryRequest::RegisterKeyboard { responder, .. }) => {
                            let _ = responder.send();
                        }
                        _ => panic!("Registry handler received an unexpected request"),
                    }
                } else {
                    panic!("Registry handler did not receive RegistryRequest")
                }
            })
        });

        let res = dev.ui_dev_create_inner(&mut locked, &current_task, Some(registry_proxy));
        assert_eq!(res, Ok(SUCCESS));

        handle.join().expect("stream panic");

        // This test will timeout if the `fuchsia.ui.test.input` stub does not
        // receive the device register request.
        // Or panic if the stub receives an invalid request.
    }

    #[::fuchsia::test]
    async fn ui_dev_create_keyboard_fails_no_registry() {
        let dev = UinputDevice::new();
        let (_kernel, current_task, file_object, mut locked) = make_kernel_objects(dev.clone());
        let address = map_memory(
            &current_task,
            UserAddress::default(),
            std::mem::size_of::<uapi::uinput_setup>() as u64,
        );
        let _ = dev.ioctl(
            &mut locked,
            &file_object,
            &current_task,
            uapi::UI_SET_EVBIT,
            SyscallArg::from(uapi::EV_KEY as u64),
        );
        let _ =
            dev.ioctl(&mut locked, &file_object, &current_task, uapi::UI_DEV_SETUP, address.into());
        let res = dev.ui_dev_create_inner(&mut locked, &current_task, None);
        assert_eq!(res, error!(EPERM))
    }

    // In practice Uinput write() will send converted events to Input Pipeline via input_helper,
    // but for testing purposes we create a mock server end and verify that it receives the
    // correct KeyboardRequests from Uinput's CreatedDevice when write() calls are made.
    #[fasync::run(2, test)]
    async fn keyboard_write() {
        let dev = UinputDevice::new();
        let (_kernel, current_task, file_object, mut locked) = make_kernel_objects(dev.clone());
        let address = map_memory(
            &current_task,
            UserAddress::default(),
            std::mem::size_of::<uapi::uinput_setup>() as u64,
        );
        let _ = dev.ioctl(
            &mut locked,
            &file_object,
            &current_task,
            uapi::UI_SET_EVBIT,
            SyscallArg::from(uapi::EV_KEY as u64),
        );
        let _ =
            dev.ioctl(&mut locked, &file_object, &current_task, uapi::UI_DEV_SETUP, address.into());

        let (key_client, mut key_server) =
            fidl::endpoints::create_sync_proxy_and_stream::<futinput::KeyboardMarker>()
                .expect("create Keyboard proxy and stream");
        dev.inner.lock().created_device =
            CreatedDevice::Keyboard(key_client, LinuxKeyboardEventParser::create());

        let (input_id, device_type) = match dev.inner.lock().get_id_and_device_type() {
            Some((id, dev)) => (id, dev),
            None => panic!("Could not get device ID and type."),
        };
        add_and_register_input_device(
            &mut locked,
            &current_task,
            VirtualDevice { input_id, device_type },
        );
        new_device();

        let expected_pressed_keys: Vec<Vec<Key>> =
            vec![vec![Key::A], vec![Key::A, Key::B], vec![Key::B], vec![]];
        let mut expected_keys = expected_pressed_keys.into_iter();

        // Spawn a separate thread for Keyboard's server to wait for KeyboardRequests
        // sent from write() calls
        let handle = thread::spawn(move || {
            fasync::LocalExecutor::new().run_singlethreaded(async {
                let mut num_received_events = 0;
                while let Some(Ok(request)) = key_server.next().await {
                    match request {
                        futinput::KeyboardRequest::SimulateKeyEvent {
                            payload:
                                KeyboardSimulateKeyEventRequest {
                                    report: Some(fir::KeyboardInputReport { pressed_keys3, .. }),
                                    ..
                                },
                            responder,
                        } => {
                            assert_eq!(pressed_keys3, expected_keys.next());
                            let _ = responder.send();
                        }
                        _ => panic!("Registry handler received an unexpected request"),
                    }
                    num_received_events += 1;

                    // 4 expected incoming events
                    if num_received_events > 3 {
                        break;
                    }
                }
            })
        });

        let mut press_a_ev = make_write_input_buffer(uapi::EV_KEY, uapi::KEY_A, 1);
        let mut press_b_ev = make_write_input_buffer(uapi::EV_KEY, uapi::KEY_B, 1);
        let mut release_a_ev = make_write_input_buffer(uapi::EV_KEY, uapi::KEY_A, 0);
        let mut release_b_ev = make_write_input_buffer(uapi::EV_KEY, uapi::KEY_B, 0);

        let sync_ev = uapi::input_event {
            type_: uapi::EV_SYN as u16,
            code: uapi::SYN_REPORT as u16,
            value: 0,
            ..Default::default()
        };

        let mut locked_write = locked.cast_locked::<WriteOps>();

        assert_write(&dev, &mut locked_write, &file_object, &current_task, 0, &mut press_a_ev);
        let mut sync_buff: VecInputBuffer = sync_ev.clone().as_bytes().to_vec().into();
        assert_write(&dev, &mut locked_write, &file_object, &current_task, 0, &mut sync_buff);

        assert_write(&dev, &mut locked_write, &file_object, &current_task, 0, &mut press_b_ev);
        sync_buff = sync_ev.clone().as_bytes().to_vec().into();
        assert_write(&dev, &mut locked_write, &file_object, &current_task, 0, &mut sync_buff);

        assert_write(&dev, &mut locked_write, &file_object, &current_task, 0, &mut release_a_ev);
        sync_buff = sync_ev.clone().as_bytes().to_vec().into();
        assert_write(&dev, &mut locked_write, &file_object, &current_task, 0, &mut sync_buff);

        assert_write(&dev, &mut locked_write, &file_object, &current_task, 0, &mut release_b_ev);
        sync_buff = sync_ev.as_bytes().to_vec().into();
        assert_write(&dev, &mut locked_write, &file_object, &current_task, 0, &mut sync_buff);

        handle.join().expect("stream panic");
    }

    #[fasync::run(2, test)]
    async fn ui_dev_create_touchscreen() {
        let dev = UinputDevice::new();
        let (_kernel, current_task, file_object, mut locked) = make_kernel_objects(dev.clone());
        let address = map_memory(
            &current_task,
            UserAddress::default(),
            std::mem::size_of::<uapi::uinput_setup>() as u64,
        );
        {
            let _ = dev.ioctl(
                &mut locked,
                &file_object,
                &current_task,
                uapi::UI_SET_EVBIT,
                SyscallArg::from(uapi::EV_ABS as u64),
            );
            let _ = dev.ioctl(
                &mut locked,
                &file_object,
                &current_task,
                uapi::UI_DEV_SETUP,
                address.into(),
            );
        }

        let (registry_proxy, mut stream) =
            create_sync_proxy_and_stream::<futinput::RegistryMarker>()
                .expect("create Registry proxy and stream");

        let handle = thread::spawn(move || {
            fasync::LocalExecutor::new().run_singlethreaded(async {
                if let Some(request) = stream.next().await {
                    match request {
                        Ok(futinput::RegistryRequest::RegisterTouchScreen {
                            responder, ..
                        }) => {
                            let _ = responder.send();
                        }
                        _ => panic!("Registry handler received an unexpected request"),
                    }
                } else {
                    panic!("Registry handler did not receive RegistryRequest")
                }
            })
        });

        let res = dev.ui_dev_create_inner(&mut locked, &current_task, Some(registry_proxy));
        assert_eq!(res, Ok(SUCCESS));

        handle.join().expect("stream panic");

        // This test will timeout if the `fuchsia.ui.test.input` stub does not
        // receive the device register request.
        // Or panic if the stub receives an invalid request.
    }

    #[::fuchsia::test]
    async fn ui_dev_create_touchscreen_fails_no_registry() {
        let dev = UinputDevice::new();
        let (_kernel, current_task, file_object, mut locked) = make_kernel_objects(dev.clone());
        let address = map_memory(
            &current_task,
            UserAddress::default(),
            std::mem::size_of::<uapi::uinput_setup>() as u64,
        );
        {
            let _ = dev.ioctl(
                &mut locked,
                &file_object,
                &current_task,
                uapi::UI_SET_EVBIT,
                SyscallArg::from(uapi::EV_ABS as u64),
            );
            let _ = dev.ioctl(
                &mut locked,
                &file_object,
                &current_task,
                uapi::UI_DEV_SETUP,
                address.into(),
            );
        }
        let res = dev.ui_dev_create_inner(&mut locked, &current_task, None);
        assert_eq!(res, error!(EPERM))
    }

    // In practice Uinput write() will send converted events to Input Pipeline via input_helper,
    // but for testing purposes we create a mock server end and verify that it receives the
    // correct TouchScreen requests from Uinput's CreatedDevice when write() calls are made.
    #[fasync::run(2, test)]
    async fn touchscreen_write() {
        let dev = UinputDevice::new();
        let (_kernel, current_task, file_object, mut locked) = make_kernel_objects(dev.clone());
        {
            let address = map_memory(
                &current_task,
                UserAddress::default(),
                std::mem::size_of::<uapi::uinput_setup>() as u64,
            );
            let _ = dev.ioctl(
                &mut locked,
                &file_object,
                &current_task,
                uapi::UI_SET_EVBIT,
                SyscallArg::from(uapi::EV_ABS as u64),
            );
            let _ = dev.ioctl(
                &mut locked,
                &file_object,
                &current_task,
                uapi::UI_DEV_SETUP,
                address.into(),
            );
        }

        let (touch_client, mut touch_server) =
            fidl::endpoints::create_sync_proxy_and_stream::<futinput::TouchScreenMarker>()
                .expect("create TouchScreen proxy and stream");
        dev.inner.lock().created_device =
            CreatedDevice::Touchscreen(touch_client, LinuxTouchEventParser::create());

        let (input_id, device_type) = match dev.inner.lock().get_id_and_device_type() {
            Some((id, dev)) => (id, dev),
            None => panic!("Could not get device ID and type."),
        };
        add_and_register_input_device(
            &mut locked,
            &current_task,
            VirtualDevice { input_id, device_type },
        );
        new_device();

        let expected_touch_contacts: Vec<Vec<fir::ContactInputReport>> = vec![
            vec![fir::ContactInputReport {
                contact_id: Some(0),
                position_x: Some(10),
                position_y: Some(20),
                ..fir::ContactInputReport::default()
            }],
            vec![fir::ContactInputReport {
                contact_id: Some(0),
                position_x: Some(10),
                position_y: Some(25),
                ..fir::ContactInputReport::default()
            }],
            vec![],
            vec![
                fir::ContactInputReport {
                    contact_id: Some(1),
                    position_x: Some(20),
                    position_y: Some(20),
                    ..fir::ContactInputReport::default()
                },
                fir::ContactInputReport {
                    contact_id: Some(2),
                    position_x: Some(30),
                    position_y: Some(20),
                    ..fir::ContactInputReport::default()
                },
            ],
            vec![
                fir::ContactInputReport {
                    contact_id: Some(1),
                    position_x: Some(25),
                    position_y: Some(30),
                    ..fir::ContactInputReport::default()
                },
                fir::ContactInputReport {
                    contact_id: Some(2),
                    position_x: Some(35),
                    position_y: Some(32),
                    ..fir::ContactInputReport::default()
                },
            ],
            vec![fir::ContactInputReport {
                contact_id: Some(2),
                position_x: Some(35),
                position_y: Some(30),
                ..fir::ContactInputReport::default()
            }],
            vec![],
        ];
        let mut expected_contacts = expected_touch_contacts.into_iter();

        // Spawn a separate thread for TouchScreen's server to wait for TouchScreenRequests
        // sent from write() calls
        let handle = thread::spawn(move || {
            fasync::LocalExecutor::new().run_singlethreaded(async {
                let mut num_received_events = 0;
                while let Some(Ok(request)) = touch_server.next().await {
                    match request {
                        futinput::TouchScreenRequest::SimulateTouchEvent {
                            report: fir::TouchInputReport { contacts, .. },
                            responder,
                        } => {
                            assert_eq!(contacts, expected_contacts.next());
                            let _ = responder.send();
                        }
                        _ => panic!("Registry handler received an unexpected request"),
                    }
                    num_received_events += 1;

                    // 7 expected incoming events
                    if num_received_events > 6 {
                        break;
                    }
                }
            })
        });

        // first contact
        let mut slot_0 = make_write_input_buffer(uapi::EV_ABS, uapi::ABS_MT_SLOT, 0);
        let mut traking_id_0 = make_write_input_buffer(uapi::EV_ABS, uapi::ABS_MT_TRACKING_ID, 0);
        let mut touch_x_0 = make_write_input_buffer(uapi::EV_ABS, uapi::ABS_MT_POSITION_X, 10);
        let mut touch_y_0 = make_write_input_buffer(uapi::EV_ABS, uapi::ABS_MT_POSITION_Y, 20);
        let sync_ev = uapi::input_event {
            type_: uapi::EV_SYN as u16,
            code: uapi::SYN_REPORT as u16,
            value: 0,
            ..Default::default()
        };
        let mut locked_write = locked.cast_locked::<WriteOps>();

        assert_write(&dev, &mut locked_write, &file_object, &current_task, 0, &mut slot_0);
        assert_write(&dev, &mut locked_write, &file_object, &current_task, 0, &mut traking_id_0);
        assert_write(&dev, &mut locked_write, &file_object, &current_task, 0, &mut touch_x_0);
        assert_write(&dev, &mut locked_write, &file_object, &current_task, 0, &mut touch_y_0);
        let mut sync_buff: VecInputBuffer = sync_ev.clone().as_bytes().to_vec().into();
        assert_write(&dev, &mut locked_write, &file_object, &current_task, 0, &mut sync_buff);

        // first contact moved
        slot_0 = make_write_input_buffer(uapi::EV_ABS, uapi::ABS_MT_SLOT, 0);
        touch_x_0 = make_write_input_buffer(uapi::EV_ABS, uapi::ABS_MT_POSITION_X, 10);
        touch_y_0 = make_write_input_buffer(uapi::EV_ABS, uapi::ABS_MT_POSITION_Y, 25);
        assert_write(&dev, &mut locked_write, &file_object, &current_task, 0, &mut slot_0);
        assert_write(&dev, &mut locked_write, &file_object, &current_task, 0, &mut touch_x_0);
        assert_write(&dev, &mut locked_write, &file_object, &current_task, 0, &mut touch_y_0);
        sync_buff = sync_ev.clone().as_bytes().to_vec().into();
        assert_write(&dev, &mut locked_write, &file_object, &current_task, 0, &mut sync_buff);

        // first contact lifted
        let mut tracking_id_lifted =
            make_write_input_buffer(uapi::EV_ABS, uapi::ABS_MT_TRACKING_ID, -1);
        slot_0 = make_write_input_buffer(uapi::EV_ABS, uapi::ABS_MT_SLOT, 0);
        assert_write(&dev, &mut locked_write, &file_object, &current_task, 0, &mut slot_0);
        assert_write(
            &dev,
            &mut locked_write,
            &file_object,
            &current_task,
            0,
            &mut tracking_id_lifted,
        );
        sync_buff = sync_ev.clone().as_bytes().to_vec().into();
        assert_write(&dev, &mut locked_write, &file_object, &current_task, 0, &mut sync_buff);

        // multi-touch contacts
        let mut slot_1 = make_write_input_buffer(uapi::EV_ABS, uapi::ABS_MT_SLOT, 1);
        let mut traking_id_1 = make_write_input_buffer(uapi::EV_ABS, uapi::ABS_MT_TRACKING_ID, 1);
        let mut touch_x_1 = make_write_input_buffer(uapi::EV_ABS, uapi::ABS_MT_POSITION_X, 20);
        let mut touch_y_1 = make_write_input_buffer(uapi::EV_ABS, uapi::ABS_MT_POSITION_Y, 20);
        let mut slot_2 = make_write_input_buffer(uapi::EV_ABS, uapi::ABS_MT_SLOT, 2);
        let mut traking_id_2 = make_write_input_buffer(uapi::EV_ABS, uapi::ABS_MT_TRACKING_ID, 2);
        let mut touch_x_2 = make_write_input_buffer(uapi::EV_ABS, uapi::ABS_MT_POSITION_X, 30);
        let mut touch_y_2 = make_write_input_buffer(uapi::EV_ABS, uapi::ABS_MT_POSITION_Y, 20);

        assert_write(&dev, &mut locked_write, &file_object, &current_task, 0, &mut slot_1);
        assert_write(&dev, &mut locked_write, &file_object, &current_task, 0, &mut traking_id_1);
        assert_write(&dev, &mut locked_write, &file_object, &current_task, 0, &mut touch_x_1);
        assert_write(&dev, &mut locked_write, &file_object, &current_task, 0, &mut touch_y_1);
        assert_write(&dev, &mut locked_write, &file_object, &current_task, 0, &mut slot_2);
        assert_write(&dev, &mut locked_write, &file_object, &current_task, 0, &mut traking_id_2);
        assert_write(&dev, &mut locked_write, &file_object, &current_task, 0, &mut touch_x_2);
        assert_write(&dev, &mut locked_write, &file_object, &current_task, 0, &mut touch_y_2);
        sync_buff = sync_ev.clone().as_bytes().to_vec().into();
        assert_write(&dev, &mut locked_write, &file_object, &current_task, 0, &mut sync_buff);

        // multi-touch contacts moved
        slot_1 = make_write_input_buffer(uapi::EV_ABS, uapi::ABS_MT_SLOT, 1);
        touch_x_1 = make_write_input_buffer(uapi::EV_ABS, uapi::ABS_MT_POSITION_X, 25);
        touch_y_1 = make_write_input_buffer(uapi::EV_ABS, uapi::ABS_MT_POSITION_Y, 30);
        slot_2 = make_write_input_buffer(uapi::EV_ABS, uapi::ABS_MT_SLOT, 2);
        touch_x_2 = make_write_input_buffer(uapi::EV_ABS, uapi::ABS_MT_POSITION_X, 35);
        touch_y_2 = make_write_input_buffer(uapi::EV_ABS, uapi::ABS_MT_POSITION_Y, 32);

        assert_write(&dev, &mut locked_write, &file_object, &current_task, 0, &mut slot_1);
        assert_write(&dev, &mut locked_write, &file_object, &current_task, 0, &mut touch_x_1);
        assert_write(&dev, &mut locked_write, &file_object, &current_task, 0, &mut touch_y_1);
        assert_write(&dev, &mut locked_write, &file_object, &current_task, 0, &mut slot_2);
        assert_write(&dev, &mut locked_write, &file_object, &current_task, 0, &mut touch_x_2);
        assert_write(&dev, &mut locked_write, &file_object, &current_task, 0, &mut touch_y_2);
        sync_buff = sync_ev.clone().as_bytes().to_vec().into();
        assert_write(&dev, &mut locked_write, &file_object, &current_task, 0, &mut sync_buff);

        // first multi-touch contact lifted
        slot_1 = make_write_input_buffer(uapi::EV_ABS, uapi::ABS_MT_SLOT, 1);
        tracking_id_lifted = make_write_input_buffer(uapi::EV_ABS, uapi::ABS_MT_TRACKING_ID, -1);
        slot_2 = make_write_input_buffer(uapi::EV_ABS, uapi::ABS_MT_SLOT, 2);
        touch_x_2 = make_write_input_buffer(uapi::EV_ABS, uapi::ABS_MT_POSITION_X, 35);
        touch_y_2 = make_write_input_buffer(uapi::EV_ABS, uapi::ABS_MT_POSITION_Y, 30);
        assert_write(&dev, &mut locked_write, &file_object, &current_task, 0, &mut slot_1);
        assert_write(
            &dev,
            &mut locked_write,
            &file_object,
            &current_task,
            0,
            &mut tracking_id_lifted,
        );
        assert_write(&dev, &mut locked_write, &file_object, &current_task, 0, &mut slot_2);
        assert_write(&dev, &mut locked_write, &file_object, &current_task, 0, &mut touch_x_2);
        assert_write(&dev, &mut locked_write, &file_object, &current_task, 0, &mut touch_y_2);
        sync_buff = sync_ev.clone().as_bytes().to_vec().into();
        assert_write(&dev, &mut locked_write, &file_object, &current_task, 0, &mut sync_buff);

        // second multi-touch contact lifted
        slot_2 = make_write_input_buffer(uapi::EV_ABS, uapi::ABS_MT_SLOT, 2);
        tracking_id_lifted = make_write_input_buffer(uapi::EV_ABS, uapi::ABS_MT_TRACKING_ID, -1);
        assert_write(&dev, &mut locked_write, &file_object, &current_task, 0, &mut slot_2);
        assert_write(
            &dev,
            &mut locked_write,
            &file_object,
            &current_task,
            0,
            &mut tracking_id_lifted,
        );
        sync_buff = sync_ev.clone().as_bytes().to_vec().into();
        assert_write(&dev, &mut locked_write, &file_object, &current_task, 0, &mut sync_buff);

        handle.join().expect("stream panic");
    }
}
