// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_inspect::{NumericProperty, Property};
use starnix_core::fileops_impl_nonseekable;
use starnix_core::mm::{MemoryAccessor, MemoryAccessorExt};
use starnix_core::task::{CurrentTask, EventHandler, WaitCanceler, WaitQueue, Waiter};
use starnix_core::vfs::buffers::{InputBuffer, OutputBuffer};
use starnix_core::vfs::{fileops_impl_noop_sync, FileObject, FileOps};
use starnix_logging::{log_info, track_stub};
use starnix_sync::{FileOpsCore, Locked, Mutex, Unlocked};
use starnix_syscalls::{SyscallArg, SyscallResult, SUCCESS};
use starnix_types::time::duration_from_timeval;
use starnix_uapi::errors::Errno;
use starnix_uapi::user_address::{UserAddress, UserRef};
use starnix_uapi::vfs::FdEvents;
use starnix_uapi::{
    error, uapi, ABS_CNT, ABS_MT_POSITION_X, ABS_MT_POSITION_Y, ABS_MT_SLOT, ABS_MT_TRACKING_ID,
    BTN_MISC, BTN_TOUCH, FF_CNT, INPUT_PROP_CNT, INPUT_PROP_DIRECT, KEY_CNT, KEY_POWER, LED_CNT,
    MSC_CNT, REL_CNT, REL_WHEEL, SW_CNT,
};
use std::collections::VecDeque;
use std::sync::Arc;
use zerocopy::IntoBytes as _; // for `as_bytes()`

const INPUT_EVENT_SIZE: usize = std::mem::size_of::<uapi::input_event>();

pub struct InputFileStatus {
    /// The number of FIDL events received by this file from Fuchsia input system.
    ///
    /// We expect:
    /// fidl_events_received_count = fidl_events_ignored_count +
    ///                              fidl_events_unexpected_count +
    ///                              fidl_events_converted_count
    /// otherwise starnix ignored events unexpectedly.
    ///
    /// fidl_events_unexpected_count should be 0, if not it hints issues from upstream of ui stack.
    pub fidl_events_received_count: fuchsia_inspect::UintProperty,

    /// The number of FIDL events ignored to this module’s representation of TouchEvent.
    pub fidl_events_ignored_count: fuchsia_inspect::UintProperty,

    /// The unexpected number of FIDL events reached to this module should be filtered out
    /// earlier in the UI stack.
    /// It maybe unexpected format or unexpected order.
    pub fidl_events_unexpected_count: fuchsia_inspect::UintProperty,

    /// The number of FIDL events converted to this module’s representation of TouchEvent.
    pub fidl_events_converted_count: fuchsia_inspect::UintProperty,

    /// The number of uapi::input_events generated from TouchEvents.
    pub uapi_events_generated_count: fuchsia_inspect::UintProperty,

    /// The event time of the last generated uapi::input_event.
    pub last_generated_uapi_event_timestamp_ns: fuchsia_inspect::IntProperty,

    /// The number of uapi::input_events read from this input file by external process.
    pub uapi_events_read_count: fuchsia_inspect::UintProperty,

    /// The event time of the last uapi::input_event read by external process.
    pub last_read_uapi_event_timestamp_ns: fuchsia_inspect::IntProperty,
}

impl InputFileStatus {
    fn new(node: &fuchsia_inspect::Node) -> Self {
        let fidl_events_received_count = node.create_uint("fidl_events_received_count", 0);
        let fidl_events_ignored_count = node.create_uint("fidl_events_ignored_count", 0);
        let fidl_events_unexpected_count = node.create_uint("fidl_events_unexpected_count", 0);
        let fidl_events_converted_count = node.create_uint("fidl_events_converted_count", 0);
        let uapi_events_generated_count = node.create_uint("uapi_events_generated_count", 0);
        let last_generated_uapi_event_timestamp_ns =
            node.create_int("last_generated_uapi_event_timestamp_ns", 0);
        let uapi_events_read_count = node.create_uint("uapi_events_read_count", 0);
        let last_read_uapi_event_timestamp_ns =
            node.create_int("last_read_uapi_event_timestamp_ns", 0);
        Self {
            fidl_events_received_count,
            fidl_events_ignored_count,
            fidl_events_unexpected_count,
            fidl_events_converted_count,
            uapi_events_generated_count,
            last_generated_uapi_event_timestamp_ns,
            uapi_events_read_count,
            last_read_uapi_event_timestamp_ns,
        }
    }

    pub fn count_received_events(&self, count: u64) {
        self.fidl_events_received_count.add(count);
    }

    pub fn count_ignored_events(&self, count: u64) {
        self.fidl_events_ignored_count.add(count);
    }

    pub fn count_unexpected_events(&self, count: u64) {
        self.fidl_events_unexpected_count.add(count);
    }

    pub fn count_converted_events(&self, count: u64) {
        self.fidl_events_converted_count.add(count);
    }

    pub fn count_generated_events(&self, count: u64, event_time_ns: i64) {
        self.uapi_events_generated_count.add(count);
        self.last_generated_uapi_event_timestamp_ns.set(event_time_ns);
    }

    pub fn count_read_events(&self, count: u64, event_time_ns: i64) {
        self.uapi_events_read_count.add(count);
        self.last_read_uapi_event_timestamp_ns.set(event_time_ns);
    }
}

pub struct InputFile {
    driver_version: u32,
    input_id: uapi::input_id,
    supported_keys: BitSet<{ min_bytes(KEY_CNT) }>,
    supported_position_attributes: BitSet<{ min_bytes(ABS_CNT) }>, // ABSolute position
    supported_motion_attributes: BitSet<{ min_bytes(REL_CNT) }>,   // RELative motion
    supported_switches: BitSet<{ min_bytes(SW_CNT) }>,
    supported_leds: BitSet<{ min_bytes(LED_CNT) }>,
    supported_haptics: BitSet<{ min_bytes(FF_CNT) }>, // 'F'orce 'F'eedback
    supported_misc_features: BitSet<{ min_bytes(MSC_CNT) }>,
    properties: BitSet<{ min_bytes(INPUT_PROP_CNT) }>,
    mt_slot_axis_info: uapi::input_absinfo,
    mt_tracking_id_axis_info: uapi::input_absinfo,
    x_axis_info: uapi::input_absinfo,
    y_axis_info: uapi::input_absinfo,
    pub inner: Mutex<InputFileMutableState>,
    // InputFile will be initialized with an InputFileStatus that holds Inspect data
    // `None` for Uinput InputFiles
    pub inspect_status: Option<Arc<InputFileStatus>>,

    // A descriptive device name. Should contain only alphanumerics and `_`.
    device_name: String,
}

// Mutable state of `InputFile`
pub struct InputFileMutableState {
    pub events: VecDeque<uapi::input_event>,
    pub waiters: WaitQueue,
}

/// Returns the minimum number of bytes required to store `n_bits` bits.
const fn min_bytes(n_bits: u32) -> usize {
    ((n_bits as usize) + 7) / 8
}

/// Returns appropriate `INPUT_PROP`-erties for a keyboard device.
fn keyboard_properties() -> BitSet<{ min_bytes(INPUT_PROP_CNT) }> {
    let mut attrs = BitSet::new();
    attrs.set(INPUT_PROP_DIRECT);
    attrs
}

/// Returns appropriate `KEY`-board related flags for a touchscreen device.
fn touch_key_attributes() -> BitSet<{ min_bytes(KEY_CNT) }> {
    let mut attrs = BitSet::new();
    attrs.set(BTN_TOUCH);
    attrs
}

/// Returns appropriate `ABS`-olute position related flags for a touchscreen device.
fn touch_position_attributes() -> BitSet<{ min_bytes(ABS_CNT) }> {
    let mut attrs = BitSet::new();
    attrs.set(ABS_MT_SLOT);
    attrs.set(ABS_MT_TRACKING_ID);
    attrs.set(ABS_MT_POSITION_X);
    attrs.set(ABS_MT_POSITION_Y);
    attrs
}

/// Returns appropriate `INPUT_PROP`-erties for a touchscreen device.
fn touch_properties() -> BitSet<{ min_bytes(INPUT_PROP_CNT) }> {
    let mut attrs = BitSet::new();
    attrs.set(INPUT_PROP_DIRECT);
    attrs
}

/// Returns appropriate `KEY`-board related flags for a keyboard device.
fn keyboard_key_attributes() -> BitSet<{ min_bytes(KEY_CNT) }> {
    let mut attrs = BitSet::new();
    attrs.set(BTN_MISC);
    attrs.set(KEY_POWER);
    attrs
}

/// Returns appropriate `ABS`-olute position related flags for a keyboard device.
fn keyboard_position_attributes() -> BitSet<{ min_bytes(ABS_CNT) }> {
    BitSet::new()
}

fn mouse_wheel_attributes() -> BitSet<{ min_bytes(REL_CNT) }> {
    let mut attrs = BitSet::new();
    attrs.set(REL_WHEEL);
    attrs
}

/// Makes a device name string from a name and device ID details.
///
/// For practical reasons the device name should contain alphanumerics and `_`.
fn get_device_name(name: &str, input_id: &uapi::input_id) -> String {
    format!("{}_{:04x}_{:04x}_v{}", name, input_id.vendor, input_id.product, input_id.version)
}

impl InputFile {
    // Per https://www.linuxjournal.com/article/6429, the driver version is 32-bits wide,
    // and interpreted as:
    // * [31-16]: version
    // * [15-08]: minor
    // * [07-00]: patch level
    const DRIVER_VERSION: u32 = 0;

    /// Creates an `InputFile` instance suitable for emulating a touchscreen.
    ///
    /// # Parameters
    /// - `input_id`: device's bustype, vendor id, product id, and version.
    /// - `width`: width of screen.
    /// - `height`: height of screen.
    /// - `inspect_status`: The inspect status for the parent device of "touch_input_file".
    pub fn new_touch(
        input_id: uapi::input_id,
        width: i32,
        height: i32,
        node: Option<&fuchsia_inspect::Node>,
    ) -> Self {
        let device_name = get_device_name("starnix_touch", &input_id);
        // Fuchsia scales the position reported by the touch sensor to fit view coordinates.
        // Hence, the range of touch positions is exactly the same as the range of view
        // coordinates.
        Self {
            driver_version: Self::DRIVER_VERSION,
            input_id,
            supported_keys: touch_key_attributes(),
            supported_position_attributes: touch_position_attributes(),
            supported_motion_attributes: BitSet::new(), // None supported, not a mouse.
            supported_switches: BitSet::new(),          // None supported
            supported_leds: BitSet::new(),              // None supported
            supported_haptics: BitSet::new(),           // None supported
            supported_misc_features: BitSet::new(),     // None supported
            properties: touch_properties(),
            mt_slot_axis_info: uapi::input_absinfo {
                minimum: 0,
                maximum: 10,
                ..uapi::input_absinfo::default()
            },
            mt_tracking_id_axis_info: uapi::input_absinfo {
                minimum: 0,
                maximum: i32::MAX,
                ..uapi::input_absinfo::default()
            },
            x_axis_info: uapi::input_absinfo {
                minimum: 0,
                maximum: i32::from(width),
                // TODO(https://fxbug.dev/42075436): `value` field should contain the most recent
                // X position.
                ..uapi::input_absinfo::default()
            },
            y_axis_info: uapi::input_absinfo {
                minimum: 0,
                maximum: i32::from(height),
                // TODO(https://fxbug.dev/42075436): `value` field should contain the most recent
                // Y position.
                ..uapi::input_absinfo::default()
            },
            inner: Mutex::new(InputFileMutableState {
                events: VecDeque::new(),
                waiters: WaitQueue::default(),
            }),
            inspect_status: node.map(|n| Arc::new(InputFileStatus::new(n))),
            device_name,
        }
    }

    /// Creates an `InputFile` instance suitable for emulating a keyboard.
    ///
    /// # Parameters
    /// - `input_id`: device's bustype, vendor id, product id, and version.
    /// - `inspect_status`: The inspect status for the parent device of "keyboard_input_file".
    pub fn new_keyboard(input_id: uapi::input_id, node: Option<&fuchsia_inspect::Node>) -> Self {
        let device_name = get_device_name("starnix_buttons", &input_id);
        Self {
            driver_version: Self::DRIVER_VERSION,
            input_id,
            supported_keys: keyboard_key_attributes(),
            supported_position_attributes: keyboard_position_attributes(),
            supported_motion_attributes: BitSet::new(), // None supported, not a mouse.
            supported_switches: BitSet::new(),          // None supported
            supported_leds: BitSet::new(),              // None supported
            supported_haptics: BitSet::new(),           // None supported
            supported_misc_features: BitSet::new(),     // None supported
            properties: keyboard_properties(),
            mt_slot_axis_info: uapi::input_absinfo::default(),
            mt_tracking_id_axis_info: uapi::input_absinfo::default(),
            x_axis_info: uapi::input_absinfo::default(),
            y_axis_info: uapi::input_absinfo::default(),
            inner: Mutex::new(InputFileMutableState {
                events: VecDeque::new(),
                waiters: WaitQueue::default(),
            }),
            inspect_status: node.map(|n| Arc::new(InputFileStatus::new(n))),
            device_name,
        }
    }

    /// Creates an `InputFile` instance suitable for emulating a mouse wheel.
    ///
    /// # Parameters
    /// - `input_id`: device's bustype, vendor id, product id, and version.
    /// - `inspect_status`: The inspect status for the parent device of "mouse_input_file".
    pub fn new_mouse(input_id: uapi::input_id, node: Option<&fuchsia_inspect::Node>) -> Self {
        let device_name = get_device_name("starnix_mouse", &input_id);
        Self {
            driver_version: Self::DRIVER_VERSION,
            input_id,
            supported_keys: BitSet::new(), // None supported, scroll only
            supported_position_attributes: BitSet::new(), // None supported, scroll only
            supported_motion_attributes: mouse_wheel_attributes(),
            supported_switches: BitSet::new(), // None supported
            supported_leds: BitSet::new(),     // None supported
            supported_haptics: BitSet::new(),  // None supported
            supported_misc_features: BitSet::new(), // None supported
            properties: BitSet::new(),         // None supported, scroll only
            mt_slot_axis_info: uapi::input_absinfo::default(),
            mt_tracking_id_axis_info: uapi::input_absinfo::default(),
            x_axis_info: uapi::input_absinfo::default(),
            y_axis_info: uapi::input_absinfo::default(),
            inner: Mutex::new(InputFileMutableState {
                events: VecDeque::new(),
                waiters: WaitQueue::default(),
            }),
            inspect_status: node.map(|n| Arc::new(InputFileStatus::new(n))),
            device_name,
        }
    }
}

// The bit-mask that removes the variable parts of the EVIOCGNAME ioctl
// request.
const EVIOCGNAME_MASK: u32 = 0b11_00_0000_0000_0000_1111_1111_1111_1111;

impl FileOps for InputFile {
    fileops_impl_nonseekable!();
    fileops_impl_noop_sync!();

    fn ioctl(
        &self,
        _locked: &mut Locked<'_, Unlocked>,
        _file: &FileObject,
        current_task: &CurrentTask,
        request: u32,
        arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        let user_addr = UserAddress::from(arg);
        match request {
            uapi::EVIOCGVERSION => {
                current_task.write_object(UserRef::new(user_addr), &self.driver_version)?;
                Ok(SUCCESS)
            }
            uapi::EVIOCGID => {
                current_task.write_object(UserRef::new(user_addr), &self.input_id)?;
                Ok(SUCCESS)
            }
            uapi::EVIOCGBIT_EV_KEY => {
                current_task.write_object(UserRef::new(user_addr), &self.supported_keys.bytes)?;
                Ok(SUCCESS)
            }
            uapi::EVIOCGBIT_EV_ABS => {
                current_task.write_object(
                    UserRef::new(user_addr),
                    &self.supported_position_attributes.bytes,
                )?;
                Ok(SUCCESS)
            }
            uapi::EVIOCGBIT_EV_REL => {
                current_task.write_object(
                    UserRef::new(user_addr),
                    &self.supported_motion_attributes.bytes,
                )?;
                Ok(SUCCESS)
            }
            uapi::EVIOCGBIT_EV_SW => {
                current_task
                    .write_object(UserRef::new(user_addr), &self.supported_switches.bytes)?;
                Ok(SUCCESS)
            }
            uapi::EVIOCGBIT_EV_LED => {
                current_task.write_object(UserRef::new(user_addr), &self.supported_leds.bytes)?;
                Ok(SUCCESS)
            }
            uapi::EVIOCGBIT_EV_FF => {
                current_task
                    .write_object(UserRef::new(user_addr), &self.supported_haptics.bytes)?;
                Ok(SUCCESS)
            }
            uapi::EVIOCGBIT_EV_MSC => {
                current_task
                    .write_object(UserRef::new(user_addr), &self.supported_misc_features.bytes)?;
                Ok(SUCCESS)
            }
            uapi::EVIOCGPROP => {
                current_task.write_object(UserRef::new(user_addr), &self.properties.bytes)?;
                Ok(SUCCESS)
            }
            uapi::EVIOCGABS_MT_SLOT => {
                current_task.write_object(UserRef::new(user_addr), &self.mt_slot_axis_info)?;
                Ok(SUCCESS)
            }
            uapi::EVIOCGABS_MT_TRACKING_ID => {
                current_task
                    .write_object(UserRef::new(user_addr), &self.mt_tracking_id_axis_info)?;
                Ok(SUCCESS)
            }
            uapi::EVIOCGABS_MT_POSITION_X => {
                current_task.write_object(UserRef::new(user_addr), &self.x_axis_info)?;
                Ok(SUCCESS)
            }
            uapi::EVIOCGABS_MT_POSITION_Y => {
                current_task.write_object(UserRef::new(user_addr), &self.y_axis_info)?;
                Ok(SUCCESS)
            }

            request_with_params => {
                // Remove the variable part of the request with params, so
                // we can identify it.
                match request_with_params & EVIOCGNAME_MASK {
                    uapi::EVIOCGNAME_0 => {
                        // Request to report the device name.
                        //
                        // An EVIOCGNAME request comes with the response buffer size encoded in
                        // bits 29..16 of the request's `u32` code.  This is in contrast to
                        // most other ioctl request codes in this file, which are fully known
                        // at compile time, so we need to decode it a bit differently from
                        // other ioctl codes.
                        //
                        // See [here][hh] the macros that do this.
                        //
                        // [hh]: https://cs.opensource.google/fuchsia/fuchsia/+/main:third_party/android/platform/bionic/libc/kernel/uapi/linux/input.h;l=82;drc=0f0c18f695543b15b852f68f297744d03d642a26
                        let device_name = &self.device_name;

                        // The lowest 14 bits of the top 16 bits are the unsigned buffer
                        // length in bytes.  While we don't use multibyte characters,
                        // make sure that all sizes below are expressed in terms of
                        // bytes, not characters.
                        let buffer_bytes_count =
                            ((request_with_params >> 16) & ((1 << 14) - 1)) as usize;

                        // Zero out the entire user buffer in case the user reads too much.
                        // Probably not needed, but I don't think it hurts.
                        current_task.zero(user_addr, buffer_bytes_count)?;
                        let device_name_as_bytes = device_name.as_bytes();

                        // Copy all bytes from device name if the buffer is large enough.
                        // If not, copy one less than the buffer size, to leave space
                        // for the final NUL.
                        let to_copy_bytes_count =
                            std::cmp::min(device_name_as_bytes.len(), buffer_bytes_count - 1);
                        current_task.write_memory(
                            user_addr,
                            &device_name_as_bytes[..to_copy_bytes_count],
                        )?;
                        // EVIOCGNAME ioctl returns the number of bytes written.
                        // Do not forget the trailing NUL.
                        Ok((to_copy_bytes_count + 1).into())
                    }
                    _ => {
                        track_stub!(
                            TODO("https://fxbug.dev/322873200"),
                            "input ioctl",
                            request_with_params
                        );
                        error!(EOPNOTSUPP)
                    }
                }
            }
        }
    }

    fn read(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        let mut inner = self.inner.lock();
        let num_events = inner.events.len();
        if num_events == 0 {
            // TODO(https://fxbug.dev/42075445): `EAGAIN` is only permitted if the file is opened
            // with `O_NONBLOCK`. Figure out what to do if the file is opened without that flag.
            log_info!("read() returning EAGAIN");
            return error!(EAGAIN);
        }

        // The limit of the buffer is determined by taking the available bytes
        // and using integer division on the size of uapi::input_event in bytes.
        // This is how many events we can write at a time, up to the amount of
        // events queued to be written.
        let limit = std::cmp::min(data.available() / INPUT_EVENT_SIZE, num_events);
        if num_events > limit {
            log_info!(
                "There was only space in the given buffer to read {} of the {} queued events. Sending a notification to prompt another read.",
                limit,
                num_events
            );
            inner.waiters.notify_fd_events(FdEvents::POLLIN);
        }
        let events: Vec<uapi::input_event> = inner.events.drain(..limit).collect::<Vec<_>>();
        let last_event_timeval = events.last().expect("events is nonempty").time;
        let last_event_time_ns = duration_from_timeval::<zx::MonotonicTimeline>(last_event_timeval)
            .unwrap()
            .into_nanos();
        self.inspect_status
            .clone()
            .map(|status| status.count_read_events(events.len() as u64, last_event_time_ns));
        data.write_all(events.as_bytes())
    }

    fn write(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        _data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        track_stub!(TODO("https://fxbug.dev/322874385"), "write() on input device");
        error!(EOPNOTSUPP)
    }

    fn wait_async(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> Option<WaitCanceler> {
        Some(self.inner.lock().waiters.wait_async_fd_events(waiter, events, handler))
    }

    fn query_events(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno> {
        Ok(if self.inner.lock().events.is_empty() { FdEvents::empty() } else { FdEvents::POLLIN })
    }
}

pub struct BitSet<const NUM_BYTES: usize> {
    bytes: [u8; NUM_BYTES],
}

impl<const NUM_BYTES: usize> BitSet<{ NUM_BYTES }> {
    pub const fn new() -> Self {
        Self { bytes: [0; NUM_BYTES] }
    }

    pub fn set(&mut self, bitnum: u32) {
        let bitnum = bitnum as usize;
        let byte = bitnum / 8;
        let bit = bitnum % 8;
        self.bytes[byte] |= 1 << bit;
    }
}
