// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use starnix_core::{
    fileops_impl_nonseekable,
    mm::MemoryAccessorExt,
    task::{CurrentTask, EventHandler, WaitCanceler, WaitQueue, Waiter},
    vfs::{
        buffers::{InputBuffer, OutputBuffer},
        FileObject, FileOps,
    },
};
use starnix_logging::{log_info, track_stub};
use starnix_sync::{FileOpsCore, Locked, Unlocked, WriteOps};
use starnix_syscalls::{SyscallArg, SyscallResult, SUCCESS};
use starnix_uapi::{
    error,
    errors::Errno,
    uapi,
    user_address::{UserAddress, UserRef},
    vfs::FdEvents,
    ABS_CNT, ABS_X, ABS_Y, BTN_MISC, BTN_TOOL_FINGER, BTN_TOUCH, FF_CNT, INPUT_PROP_CNT,
    INPUT_PROP_DIRECT, KEY_CNT, KEY_POWER, LED_CNT, MSC_CNT, REL_CNT, SW_CNT,
};

use fuchsia_inspect::NumericProperty;
use starnix_sync::Mutex;
use std::collections::VecDeque;
use zerocopy::AsBytes as _; // for `as_bytes()`

pub struct InspectStatus {
    /// A node that contains the state below.
    _inspect_node: fuchsia_inspect::Node,

    /// The number of FIDL events received from Fuchsia input system.
    pub fidl_events_received_count: fuchsia_inspect::UintProperty,

    /// The number of FIDL events converted to this module’s representation of TouchEvent.
    pub fidl_events_converted_count: fuchsia_inspect::UintProperty,

    /// The number of uapi::input_events generated from TouchEvents.
    pub uapi_events_generated_count: fuchsia_inspect::UintProperty,

    /// The number of uapi::input_events read from this touch file by external process.
    pub uapi_events_read_count: fuchsia_inspect::UintProperty,
}

impl InspectStatus {
    fn new(node: fuchsia_inspect::Node) -> Self {
        let fidl_events_received_count = node.create_uint("fidl_events_received_count", 0);
        let fidl_events_converted_count = node.create_uint("fidl_events_converted_count", 0);
        let uapi_events_generated_count = node.create_uint("uapi_events_generated_count", 0);
        let uapi_events_read_count = node.create_uint("uapi_events_read_count", 0);
        Self {
            _inspect_node: node,
            fidl_events_received_count,
            fidl_events_converted_count,
            uapi_events_generated_count,
            uapi_events_read_count,
        }
    }

    pub fn count_received_events(&self, count: u64) {
        self.fidl_events_received_count.add(count);
    }

    pub fn count_converted_events(&self, count: u64) {
        self.fidl_events_converted_count.add(count);
    }

    pub fn count_generated_events(&self, count: u64) {
        self.uapi_events_generated_count.add(count);
    }

    pub fn count_read_events(&self, count: u64) {
        self.uapi_events_read_count.add(count);
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
    x_axis_info: uapi::input_absinfo,
    y_axis_info: uapi::input_absinfo,
    pub inner: Mutex<InputFileMutableState>,
}

// Mutable state of `InputFile`
pub struct InputFileMutableState {
    pub events: VecDeque<uapi::input_event>,
    pub waiters: WaitQueue,
    // TODO: https://fxbug.dev/42081918 - remove `Optional` when implementing Inspect for Keyboard InputFiles
    // Touch InputFile will be initialized with a InspectStatus that holds Inspect data
    // `None` for Keyboard InputFile
    pub inspect_status: Option<InspectStatus>,
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
    attrs.set(BTN_TOOL_FINGER);
    attrs
}

/// Returns appropriate `ABS`-olute position related flags for a touchscreen device.
fn touch_position_attributes() -> BitSet<{ min_bytes(ABS_CNT) }> {
    let mut attrs = BitSet::new();
    attrs.set(ABS_X);
    attrs.set(ABS_Y);
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
    /// - `inspect_node`: The root node for "touch_input_file" inspect tree.
    pub fn new_touch(
        input_id: uapi::input_id,
        width: i32,
        height: i32,
        inspect_node: Option<fuchsia_inspect::Node>,
    ) -> Self {
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
                inspect_status: inspect_node.map(|n| InspectStatus::new(n)),
            }),
        }
    }

    /// Creates an `InputFile` instance suitable for emulating a keyboard.
    ///
    /// # Parameters
    /// - `input_id`: device's bustype, vendor id, product id, and version.
    pub fn new_keyboard(
        input_id: uapi::input_id,
        inspect_node: Option<fuchsia_inspect::Node>,
    ) -> Self {
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
            x_axis_info: uapi::input_absinfo::default(),
            y_axis_info: uapi::input_absinfo::default(),
            inner: Mutex::new(InputFileMutableState {
                events: VecDeque::new(),
                waiters: WaitQueue::default(),
                inspect_status: inspect_node.map(|n| InspectStatus::new(n)),
            }),
        }
    }
}

impl FileOps for InputFile {
    fileops_impl_nonseekable!();

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
            uapi::EVIOCGABS_X => {
                current_task.write_object(UserRef::new(user_addr), &self.x_axis_info)?;
                Ok(SUCCESS)
            }
            uapi::EVIOCGABS_Y => {
                current_task.write_object(UserRef::new(user_addr), &self.y_axis_info)?;
                Ok(SUCCESS)
            }
            _ => {
                track_stub!(TODO("https://fxbug.dev/322873200"), "input ioctl", request);
                error!(EOPNOTSUPP)
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
        let event = inner.events.pop_front();
        match event {
            Some(event) => {
                inner.inspect_status.as_ref().map(|status| status.count_read_events(1));
                drop(inner);
                // TODO(https://fxbug.dev/42075443): Consider sending as many events as will fit
                // in `data`, instead of sending them one at a time.
                data.write_all(event.as_bytes())
            }
            // TODO(https://fxbug.dev/42075445): `EAGAIN` is only permitted if the file is opened
            // with `O_NONBLOCK`. Figure out what to do if the file is opened without that flag.
            None => {
                log_info!("read() returning EAGAIN");
                error!(EAGAIN)
            }
        }
    }

    fn write(
        &self,
        _locked: &mut Locked<'_, WriteOps>,
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
