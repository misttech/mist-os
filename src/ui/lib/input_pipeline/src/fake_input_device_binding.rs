// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::input_device, crate::keyboard_binding, async_trait::async_trait,
    futures::channel::mpsc::UnboundedSender,
};

/// A fake [`InputDeviceBinding`] for testing.
pub struct FakeInputDeviceBinding {
    /// The channel to stream InputEvents to.
    event_sender: UnboundedSender<input_device::InputEvent>,
}

#[allow(dead_code)]
impl FakeInputDeviceBinding {
    pub fn new(input_event_sender: UnboundedSender<input_device::InputEvent>) -> Self {
        FakeInputDeviceBinding { event_sender: input_event_sender }
    }
}

#[async_trait]
impl input_device::InputDeviceBinding for FakeInputDeviceBinding {
    fn get_device_descriptor(&self) -> input_device::InputDeviceDescriptor {
        input_device::InputDeviceDescriptor::Keyboard(keyboard_binding::KeyboardDeviceDescriptor {
            keys: vec![],
            device_info: fidl_fuchsia_input_report::DeviceInfo {
                vendor_id: 42,
                product_id: 43,
                version: 44,
                polling_rate: 1000,
            },
            // Random fake identifier.
            device_id: 442,
        })
    }

    fn input_event_sender(&self) -> UnboundedSender<input_device::InputEvent> {
        self.event_sender.clone()
    }
}
