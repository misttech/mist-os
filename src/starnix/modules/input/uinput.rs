// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    InputEventsRelay, InputFile, LinuxKeyboardEventParser, LinuxTouchEventParser, OpenedFiles,
};
use bit_vec::BitVec;
use fidl_fuchsia_ui_test_input::{
    self as futinput, KeyboardSimulateKeyEventRequest,
    RegistryRegisterKeyboardAndGetDeviceInfoRequest,
    RegistryRegisterTouchScreenAndGetDeviceInfoRequest,
};

use starnix_core::device::kobject::{Device, DeviceMetadata};
use starnix_core::device::{DeviceMode, DeviceOps};
use starnix_core::fileops_impl_seekless;
use starnix_core::fs::sysfs::DeviceDirectory;
use starnix_core::mm::MemoryAccessorExt;
use starnix_core::task::CurrentTask;
use starnix_core::vfs::{
    self, default_ioctl, fileops_impl_noop_sync, FileObject, FileOps, FsNode, FsString,
};
use starnix_logging::log_warn;
use starnix_sync::{DeviceOpen, FileOpsCore, LockBefore, Locked, Mutex, Unlocked};
use starnix_syscalls::{SyscallArg, SyscallResult, SUCCESS};
use starnix_uapi::device_type::INPUT_MAJOR;
use starnix_uapi::errors::Errno;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::user_address::UserRef;
use starnix_uapi::{device_type, errno, error, uapi};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;
use zerocopy::FromBytes;

// Return the current uinput API version 5, it also told caller this uinput
// supports UI_DEV_SETUP.
const UINPUT_VERSION: u32 = 5;

#[derive(Clone)]
enum DeviceType {
    Keyboard,
    Touchscreen,
}

pub fn register_uinput_device(
    locked: &mut Locked<'_, Unlocked>,
    system_task: &CurrentTask,
    input_event_relay: Arc<InputEventsRelay>,
) {
    let kernel = system_task.kernel();
    let registry = &kernel.device_registry;
    let misc_class = registry.objects.misc_class();
    let device = UinputDevice::new(input_event_relay);
    registry.register_device(
        locked,
        system_task,
        "uinput".into(),
        DeviceMetadata::new("uinput".into(), device_type::DeviceType::UINPUT, DeviceMode::Char),
        misc_class,
        DeviceDirectory::new,
        device,
    );
}

fn add_and_register_input_device<L>(
    locked: &mut Locked<'_, L>,
    system_task: &CurrentTask,
    dev_ops: impl DeviceOps,
    device_id: u32,
) -> Device
where
    L: LockBefore<FileOpsCore>,
{
    let kernel = system_task.kernel();
    let registry = &kernel.device_registry;

    let input_class = registry.objects.input_class();

    registry.register_device(
        locked,
        system_task,
        FsString::from(format!("event{}", device_id)).as_ref(),
        DeviceMetadata::new(
            format!("input/event{}", device_id).into(),
            starnix_uapi::device_type::DeviceType::new(INPUT_MAJOR, device_id),
            DeviceMode::Char,
        ),
        input_class,
        DeviceDirectory::new,
        dev_ops,
    )
}

#[derive(Clone)]
struct UinputDevice {
    input_event_relay: Arc<InputEventsRelay>,
}

impl UinputDevice {
    pub fn new(input_event_relay: Arc<InputEventsRelay>) -> Self {
        Self { input_event_relay }
    }
}

impl DeviceOps for UinputDevice {
    fn open(
        &self,
        _locked: &mut Locked<'_, DeviceOpen>,
        _current_task: &CurrentTask,
        _id: device_type::DeviceType,
        _node: &FsNode,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new(UinputDeviceFile::new(self.input_event_relay.clone())))
    }
}

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
    device_id: Option<u32>,
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

struct UinputDeviceFile {
    input_event_relay: Arc<InputEventsRelay>,
    inner: Mutex<UinputDeviceMutableState>,
}

impl UinputDeviceFile {
    pub fn new(input_event_relay: Arc<InputEventsRelay>) -> Self {
        Self {
            input_event_relay,
            inner: Mutex::new(UinputDeviceMutableState {
                enabled_evbits: BitVec::from_elem(uapi::EV_CNT as usize, false),
                input_id: None,
                created_device: CreatedDevice::None,
                k_device: None,
                device_id: None,
            }),
        }
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

                let open_files: OpenedFiles = Default::default();

                let registered_device_id = match device_type {
                    DeviceType::Keyboard => {
                        let (key_client, key_server) =
                            fidl::endpoints::create_sync_proxy::<futinput::KeyboardMarker>();
                        inner.created_device =
                            CreatedDevice::Keyboard(key_client, LinuxKeyboardEventParser::create());

                        // Register a keyboard
                        let register_res = proxy.register_keyboard_and_get_device_info(
                            RegistryRegisterKeyboardAndGetDeviceInfoRequest {
                                device: Some(key_server),
                                ..Default::default()
                            },
                            zx::MonotonicInstant::INFINITE,
                        );

                        match register_res {
                            Ok(resp) => match resp.device_id {
                                Some(device_id) => {
                                    inner.device_id = Some(device_id);
                                    self.input_event_relay.add_keyboard_device(
                                        device_id,
                                        open_files.clone(),
                                        None,
                                    );
                                    device_id
                                }
                                None => {
                                    log_warn!("register_keyboard_and_get_device_info response does not include a device_id");
                                    return error!(EPERM);
                                }
                            },
                            Err(e) => {
                                log_warn!(
                                    "Uinput could not register Keyboard device to Registry: {:?}",
                                    e
                                );
                                return error!(EPERM);
                            }
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
                        let register_res = proxy.register_touch_screen_and_get_device_info(
                            RegistryRegisterTouchScreenAndGetDeviceInfoRequest {
                                device: Some(touch_server),
                                ..Default::default()
                            },
                            zx::Instant::INFINITE,
                        );

                        match register_res {
                            Ok(resp) => match resp.device_id {
                                Some(device_id) => {
                                    inner.device_id = Some(device_id);
                                    self.input_event_relay.add_touch_device(
                                        device_id,
                                        open_files.clone(),
                                        None,
                                    );
                                    device_id
                                }
                                None => {
                                    log_warn!("register_touch_screen_and_get_device_info response does not include a device_id");
                                    return error!(EPERM);
                                }
                            },
                            Err(e) => {
                                log_warn!(
                                    "Uinput could not register Keyboard device to Registry: {:?}",
                                    e
                                );
                                return error!(EPERM);
                            }
                        }
                    }
                };

                let device = add_and_register_input_device(
                    locked,
                    current_task,
                    VirtualDevice { input_id, device_type, open_files },
                    registered_device_id,
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
        match inner.device_id {
            Some(device_id) => {
                self.input_event_relay.remove_device(device_id);
            }
            None => {
                // This is possible if caller does not call create device but calls destroy.
                // No cleanup is needed for input event relay in this case.
            }
        }

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
static COUNT_OF_UINPUT_DEVICE: AtomicI32 = AtomicI32::new(0);

fn new_device() {
    let _ = COUNT_OF_UINPUT_DEVICE.fetch_add(1, Ordering::SeqCst);
}

fn destroy_device() {
    let _ = COUNT_OF_UINPUT_DEVICE.fetch_sub(1, Ordering::SeqCst);
}

pub fn uinput_running() -> bool {
    COUNT_OF_UINPUT_DEVICE.load(Ordering::SeqCst) > 0
}

impl FileOps for UinputDeviceFile {
    fileops_impl_seekless!();
    fileops_impl_noop_sync!();

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
                default_ioctl(file, locked, current_task, request, arg)
            }
        }
    }

    fn write(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &vfs::FileObject,
        _current_task: &starnix_core::task::CurrentTask,
        _offset: usize,
        data: &mut dyn vfs::buffers::InputBuffer,
    ) -> Result<usize, Errno> {
        let content = data.read_all()?;
        let event = uapi::input_event::read_from_bytes(&content).map_err(|_| errno!(EINVAL))?;

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
                                zx::MonotonicInstant::INFINITE,
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
                            let res = proxy.simulate_touch_event(
                                &touch_report,
                                zx::MonotonicInstant::INFINITE,
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
            CreatedDevice::None => return error!(EINVAL),
        }

        Ok(content.len())
    }

    fn read(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &vfs::FileObject,
        _current_task: &starnix_core::task::CurrentTask,
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
    open_files: OpenedFiles,
}

impl DeviceOps for VirtualDevice {
    fn open(
        &self,
        _locked: &mut Locked<'_, DeviceOpen>,
        _current_task: &CurrentTask,
        _id: device_type::DeviceType,
        _node: &FsNode,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        let input_file = match &self.device_type {
            DeviceType::Keyboard => Arc::new(InputFile::new_keyboard(self.input_id, None)),
            DeviceType::Touchscreen => {
                // TODO(b/304595635): Check if screen size is required.
                Arc::new(InputFile::new_touch(self.input_id, 1000, 1000, None))
            }
        };

        self.open_files.lock().push(Arc::downgrade(&input_file));

        Ok(Box::new(input_file))
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use starnix_core::task::Kernel;
    use starnix_core::testing::{create_kernel_task_and_unlocked, AutoReleasableTask};
    use starnix_core::vfs::FileHandle;
    use std::ops::Deref;
    use std::sync::Arc;
    use test_case::test_case;

    fn make_kernel_objects<'l>(
        file: Arc<UinputDeviceFile>,
    ) -> (impl Deref<Target = Arc<Kernel>>, AutoReleasableTask, FileHandle, Locked<'l, Unlocked>)
    {
        let (kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let file_object = FileObject::new(
            &current_task,
            Box::new(file),
            // The input node doesn't really live at the root of the filesystem.
            // But the test doesn't need to be 100% representative of production.
            current_task
                .lookup_path_from_root(&mut locked, ".".into())
                .expect("failed to get namespace node for root"),
            OpenFlags::empty(),
        )
        .expect("FileObject::new failed");
        (kernel, current_task, file_object, locked)
    }

    #[test_case(uapi::EV_KEY, vec![uapi::EV_KEY as usize] => Ok(SUCCESS))]
    #[test_case(uapi::EV_ABS, vec![uapi::EV_ABS as usize] => Ok(SUCCESS))]
    #[test_case(uapi::EV_REL, vec![] => error!(EPERM))]
    #[::fuchsia::test]
    async fn ui_set_evbit(bit: u32, expected_evbits: Vec<usize>) -> Result<SyscallResult, Errno> {
        let dev = Arc::new(UinputDeviceFile::new(InputEventsRelay::new()));
        let (_kernel, current_task, file_object, mut locked) = make_kernel_objects(dev.clone());
        let mut locked = locked.cast_locked::<Unlocked>();
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
        let dev = Arc::new(UinputDeviceFile::new(InputEventsRelay::new()));
        let (_kernel, current_task, file_object, mut locked) = make_kernel_objects(dev.clone());
        let mut locked = locked.cast_locked::<Unlocked>();
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
        let dev = Arc::new(UinputDeviceFile::new(InputEventsRelay::new()));
        let (_kernel, current_task, file_object, mut locked) = make_kernel_objects(dev.clone());
        let mut locked = locked.cast_locked::<Unlocked>();
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
        let dev = Arc::new(UinputDeviceFile::new(InputEventsRelay::new()));
        let (_kernel, current_task, file_object, mut locked) = make_kernel_objects(dev.clone());
        let mut locked = locked.cast_locked::<Unlocked>();
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
        let dev = Arc::new(UinputDeviceFile::new(InputEventsRelay::new()));
        let (_kernel, current_task, file_object, mut locked) = make_kernel_objects(dev.clone());
        let mut locked = locked.cast_locked::<Unlocked>();
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
}
