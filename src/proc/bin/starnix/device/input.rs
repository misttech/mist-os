// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::device::DeviceOps;
use crate::fs::buffers::{InputBuffer, OutputBuffer};
use crate::fs::*;
use crate::logging::*;
use crate::mm::MemoryAccessorExt;
use crate::syscalls::SyscallResult;
use crate::syscalls::SUCCESS;
use crate::task::{CurrentTask, EventHandler, WaitKey, Waiter};
use crate::types::*;

use std::sync::Arc;

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
    waiter: Waiter,
}

impl InputFile {
    // Per https://www.linuxjournal.com/article/6429, the driver version is 32-bits wide,
    // and interpreted as:
    // * [31-16]: version
    // * [15-08]: minor
    // * [07-00]: patch level
    const DRIVER_VERSION: u32 = 0;

    // Per https://www.linuxjournal.com/article/6429, the bus type should be populated with a
    // sensible value, but other fields may not be.
    const INPUT_ID: uapi::input_id =
        uapi::input_id { bustype: BUS_VIRTUAL as u16, product: 0, vendor: 0, version: 0 };

    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            driver_version: Self::DRIVER_VERSION,
            input_id: Self::INPUT_ID,
            supported_keys: touch_key_attributes(),
            supported_position_attributes: touch_position_attributes(),
            supported_motion_attributes: BitSet::new(), // None supported, not a mouse.
            supported_switches: BitSet::new(),          // None supported
            supported_leds: BitSet::new(),              // None supported
            supported_haptics: BitSet::new(),           // None supported
            supported_misc_features: BitSet::new(),     // None supported
            properties: touch_properties(),
            waiter: Waiter::new(),
        })
    }
}

impl DeviceOps for Arc<InputFile> {
    fn open(
        &self,
        _current_task: &CurrentTask,
        _dev: DeviceType,
        _node: &FsNode,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new(Arc::clone(self)))
    }
}

impl FileOps for Arc<InputFile> {
    fileops_impl_nonseekable!();

    fn ioctl(
        &self,
        _file: &FileObject,
        current_task: &CurrentTask,
        request: u32,
        user_addr: UserAddress,
    ) -> Result<SyscallResult, Errno> {
        match request {
            uapi::EVIOCGVERSION => {
                current_task.mm.write_object(UserRef::new(user_addr), &self.driver_version)?;
                Ok(SUCCESS)
            }
            uapi::EVIOCGID => {
                current_task.mm.write_object(UserRef::new(user_addr), &self.input_id)?;
                Ok(SUCCESS)
            }
            uapi::EVIOCGBIT_EV_KEY => {
                current_task
                    .mm
                    .write_object(UserRef::new(user_addr), &self.supported_keys.bytes)?;
                Ok(SUCCESS)
            }
            uapi::EVIOCGBIT_EV_ABS => {
                current_task.mm.write_object(
                    UserRef::new(user_addr),
                    &self.supported_position_attributes.bytes,
                )?;
                Ok(SUCCESS)
            }
            uapi::EVIOCGBIT_EV_REL => {
                current_task.mm.write_object(
                    UserRef::new(user_addr),
                    &self.supported_motion_attributes.bytes,
                )?;
                Ok(SUCCESS)
            }
            uapi::EVIOCGBIT_EV_SW => {
                current_task
                    .mm
                    .write_object(UserRef::new(user_addr), &self.supported_switches.bytes)?;
                Ok(SUCCESS)
            }
            uapi::EVIOCGBIT_EV_LED => {
                current_task
                    .mm
                    .write_object(UserRef::new(user_addr), &self.supported_leds.bytes)?;
                Ok(SUCCESS)
            }
            uapi::EVIOCGBIT_EV_FF => {
                current_task
                    .mm
                    .write_object(UserRef::new(user_addr), &self.supported_haptics.bytes)?;
                Ok(SUCCESS)
            }
            uapi::EVIOCGBIT_EV_MSC => {
                current_task
                    .mm
                    .write_object(UserRef::new(user_addr), &self.supported_misc_features.bytes)?;
                Ok(SUCCESS)
            }
            uapi::EVIOCGPROP => {
                current_task.mm.write_object(UserRef::new(user_addr), &self.properties.bytes)?;
                Ok(SUCCESS)
            }
            _ => {
                not_implemented!(current_task, "ioctl() {} on input device", request);
                error!(EOPNOTSUPP)
            }
        }
    }

    fn read(
        &self,
        _file: &FileObject,
        current_task: &CurrentTask,
        _data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        not_implemented!(current_task, "read() on input device");
        error!(EOPNOTSUPP)
    }

    fn write(
        &self,
        _file: &FileObject,
        current_task: &CurrentTask,
        _data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        not_implemented!(current_task, "write() on input device");
        error!(EOPNOTSUPP)
    }

    fn wait_async(
        &self,
        _file: &FileObject,
        current_task: &CurrentTask,
        _waiter: &Waiter,
        _events: FdEvents,
        _handler: EventHandler,
    ) -> WaitKey {
        not_implemented!(current_task, "wait_async() on input device will block forever");
        self.waiter.fake_wait()
    }

    fn cancel_wait(&self, current_task: &CurrentTask, _waiter: &Waiter, _key: WaitKey) {
        not_implemented!(current_task, "cancel_wait() on input device");
    }

    fn query_events(&self, current_task: &CurrentTask) -> FdEvents {
        not_implemented!(current_task, "query_events() on input device");
        FdEvents::empty()
    }
}

struct BitSet<const NUM_BYTES: usize> {
    bytes: [u8; NUM_BYTES],
}

impl<const NUM_BYTES: usize> BitSet<{ NUM_BYTES }> {
    const fn new() -> Self {
        Self { bytes: [0; NUM_BYTES] }
    }

    fn set(&mut self, bitnum: u32) {
        let bitnum = bitnum as usize;
        let byte = bitnum / 8;
        let bit = bitnum % 8;
        self.bytes[byte] |= 1 << bit;
    }
}

/// Returns the minimum number of bytes required to store `n_bits` bits.
const fn min_bytes(n_bits: u32) -> usize {
    ((n_bits as usize) + 7) / 8
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
