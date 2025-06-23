// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Result};
use fuchsia_async::{Task, Timer};
use std::collections::BTreeSet;
use std::time::Duration;
pub use usb_rs::bulk_interface::BulkInterface as Interface;
use usb_rs::enumerate_devices;

// USB fastboot interface IDs
const FASTBOOT_USB_INTERFACE_CLASS: u8 = 0xff;
const FASTBOOT_USB_INTERFACE_SUBCLASS: u8 = 0x42;
const FASTBOOT_USB_INTERFACE_PROTOCOL: u8 = 0x03;

// Vendor ID
const USB_DEV_VENDOR: u16 = 0x18d1;

/// Loops continually testing if the usb device with given `serial` is
/// accepting fastboot connections, returning when it does.
///
/// Sleeps for `sleep_interval` between tests
pub async fn wait_for_live(
    serial: &str,
    tester: &mut impl FastbootUsbLiveTester,
    sleep_interval: Duration,
) {
    loop {
        if tester.is_fastboot_usb_live(serial).await {
            return;
        }
        Timer::new(sleep_interval).await;
    }
}

#[allow(async_fn_in_trait)]
pub trait FastbootUsbLiveTester: Send + 'static {
    /// Checks if the interface with the given serial number is Ready to accept
    /// fastboot commands
    async fn is_fastboot_usb_live(&mut self, serial: &str) -> bool;
}

pub struct GetVarFastbootUsbLiveTester;

impl FastbootUsbLiveTester for GetVarFastbootUsbLiveTester {
    async fn is_fastboot_usb_live(&mut self, serial: &str) -> bool {
        open_interface_with_serial(serial).await.is_ok()
    }
}

#[allow(async_fn_in_trait)]
pub trait FastbootUsbTester: Send + 'static {
    /// Checks if the interface with the given serial number is in Fastboot
    async fn is_fastboot_usb(&mut self, serial: &str) -> bool;
}

/// Checks if the USB Interface for the given serial is live and a fastboot match
/// by inspecting the Interface Information.
///
/// This does not mean that the device is ready to respond to any fastboot commands,
/// only that the USB Device with the given serial number exists and declares itself
/// to be a Fastboot device.
///
/// This calls AsyncInterface::check which does _not_ drain the USB's device's
/// buffer upon connecting
pub struct UnversionedFastbootUsbTester;

impl FastbootUsbTester for UnversionedFastbootUsbTester {
    async fn is_fastboot_usb(&mut self, _serial: &str) -> bool {
        true
    }
}

pub struct FastbootUsbWatcher {
    // Task for the discovery loop
    discovery_task: Option<Task<()>>,
    // Task for the drain loop
    drain_task: Option<Task<()>>,
}

#[derive(Debug, PartialEq)]
pub enum FastbootEvent {
    Discovered(String),
    Lost(String),
}

#[allow(async_fn_in_trait)]
pub trait FastbootEventHandler: Send + 'static {
    /// Handles an event.
    async fn handle_event(&mut self, event: Result<FastbootEvent>);
}

impl<F> FastbootEventHandler for F
where
    F: FnMut(Result<FastbootEvent>) -> () + Send + 'static,
{
    async fn handle_event(&mut self, x: Result<FastbootEvent>) -> () {
        self(x)
    }
}

#[allow(async_fn_in_trait)]
pub trait SerialNumberFinder: Send + 'static {
    async fn find_serial_numbers(&mut self) -> Vec<String>;
}

pub struct DefaultSerialFinder {}
impl SerialNumberFinder for DefaultSerialFinder {
    async fn find_serial_numbers(&mut self) -> Vec<String> {
        find_serial_numbers().await
    }
}

pub fn recommended_watcher<F>(event_handler: F) -> FastbootUsbWatcher
where
    F: FastbootEventHandler,
{
    FastbootUsbWatcher::new(
        event_handler,
        DefaultSerialFinder {},
        UnversionedFastbootUsbTester {},
        Duration::from_secs(1),
    )
}

impl FastbootUsbWatcher {
    pub fn new<F, W, O>(event_handler: F, finder: W, opener: O, interval: Duration) -> Self
    where
        F: FastbootEventHandler,
        W: SerialNumberFinder,
        O: FastbootUsbTester,
    {
        let mut res = Self { discovery_task: None, drain_task: None };

        let (sender, receiver) = async_channel::bounded::<FastbootEvent>(1);

        res.discovery_task.replace(Task::local(discovery_loop(sender, finder, opener, interval)));
        res.drain_task.replace(Task::local(handle_events_loop(receiver, event_handler)));

        res
    }
}

async fn discovery_loop<F, O>(
    events_out: async_channel::Sender<FastbootEvent>,
    mut finder: F,
    mut opener: O,
    discovery_interval: Duration,
) -> ()
where
    F: SerialNumberFinder,
    O: FastbootUsbTester,
{
    let mut serials = BTreeSet::<String>::new();
    loop {
        // Enumerate interfaces
        let new_serials = finder.find_serial_numbers().await;
        let new_serials = BTreeSet::from_iter(new_serials);
        log::debug!("found serials: {:#?}", new_serials);
        // Update Cache
        for serial in &new_serials {
            // Just because the serial is found doesnt mean that the target is ready
            if !opener.is_fastboot_usb(serial.as_str()).await {
                log::debug!(
                    "Skipping adding serial number: {serial} as it is not a Fastboot interface"
                );
                continue;
            }

            log::debug!("Inserting new serial: {}", serial);
            if serials.insert(serial.clone()) {
                log::debug!("Sending discovered event for serial: {}", serial);
                let _ = events_out.send(FastbootEvent::Discovered(serial.clone())).await;
                log::trace!("Sent discovered event for serial: {}", serial);
            }
        }

        // Check for any missing Serials
        let missing_serials: Vec<_> = serials.difference(&new_serials).cloned().collect();
        log::trace!("missing serials: {:#?}", missing_serials);
        for serial in missing_serials {
            serials.remove(&serial);
            log::trace!("Sending lost event for serial: {}", serial);
            let _ = events_out.send(FastbootEvent::Lost(serial.clone())).await;
            log::trace!("Sent lost event for serial: {}", serial);
        }

        log::trace!("discovery loop... waiting for {:#?}", discovery_interval);
        Timer::new(discovery_interval).await;
    }
}

async fn handle_events_loop<F>(receiver: async_channel::Receiver<FastbootEvent>, mut handler: F)
where
    F: FastbootEventHandler,
{
    loop {
        let event = receiver.recv().await.map_err(|e| anyhow!(e));
        log::trace!("Event loop received event: {:#?}", event);
        handler.handle_event(event).await;
    }
}

fn device_is_fastboot(
    device: &usb_rs::DeviceHandle,
    usb_device: &usb_rs::DeviceDescriptor,
    interface: &usb_rs::InterfaceDescriptor,
) -> bool {
    let subclass_match = u16::from(usb_device.vendor) == USB_DEV_VENDOR
        && u8::from(interface.class) == FASTBOOT_USB_INTERFACE_CLASS
        && u8::from(interface.subclass) == FASTBOOT_USB_INTERFACE_SUBCLASS;
    let protocol_match = u8::from(interface.protocol) == FASTBOOT_USB_INTERFACE_PROTOCOL;
    log::debug!(
        "Device: {:?} subclass_match: {}, protocol_match: {}",
        device,
        subclass_match,
        protocol_match
    );
    subclass_match && protocol_match
}

/// How many URBs to allocate for each device we communicate with.
const URB_POOL_SIZE: usize = 32;

async fn find_serial_numbers() -> Vec<String> {
    log::debug!("finding serial numbers");
    let mut serials = Vec::new();

    let Ok(devices) = enumerate_devices() else {
        return serials;
    };
    for device in devices {
        if let Some(serial) = device.serial() {
            let valid = match device.scan_interfaces(URB_POOL_SIZE, |usb_device, interface| {
                device_is_fastboot(&device, usb_device, interface)
            }) {
                Ok(_) => true,
                Err(usb_rs::Error::InterfaceNotFound) => {
                    log::warn!(device = device.debug_name().as_str(); "Interface not found");
                    false
                }
                Err(e) => {
                    log::warn!(device = device.debug_name().as_str(), error:? = e;
                                   "Error scanning USB device");
                    false
                }
            };
            log::info!("Serial: {:?} valid: {}", device.serial(), valid);
            if valid {
                serials.push(serial);
            }
        }
    }
    log::info!("About to return serials: {:?}", serials);
    return serials;
}

pub async fn open_interface_with_serial<P>(serial: P) -> Result<Interface>
where
    P: AsRef<str>,
{
    let devices = enumerate_devices()?;
    for device in devices {
        if device.serial() == Some(serial.as_ref().to_string()) {
            // Okay we match on serial number lets scan the interfaces
            let interface = match device.scan_interfaces(URB_POOL_SIZE, |usb_device, interface| {
                device_is_fastboot(&device, usb_device, interface)
            }) {
                Ok(iface) => iface,
                Err(usb_rs::Error::InterfaceNotFound) => {
                    return Err(anyhow!("Interface not found"));
                }
                Err(e) => {
                    log::warn!(device = device.debug_name().as_str(), error:? = e;
                                   "Error scanning USB device");
                    return Err(e.into());
                }
            };
            return Ok(Interface::new(interface));
        }
    }
    Err(anyhow!("Interface not found"))
}

#[cfg(test)]
mod test {
    use super::*;
    use futures::channel::mpsc::unbounded;
    use pretty_assertions::assert_eq;
    use std::collections::{HashMap, VecDeque};
    use std::sync::{Arc, Mutex};

    struct TestFastbootUsbTester {
        serial_to_is_fastboot: HashMap<String, bool>,
    }

    impl FastbootUsbTester for TestFastbootUsbTester {
        async fn is_fastboot_usb(&mut self, _serial: &str) -> bool {
            *self.serial_to_is_fastboot.get(_serial).unwrap()
        }
    }

    struct TestSerialNumberFinder {
        responses: Vec<Vec<String>>,
        is_empty: Arc<Mutex<bool>>,
    }

    impl SerialNumberFinder for TestSerialNumberFinder {
        async fn find_serial_numbers(&mut self) -> Vec<String> {
            if let Some(res) = self.responses.pop() {
                res
            } else {
                let mut lock = self.is_empty.lock().unwrap();
                *lock = true;
                vec![]
            }
        }
    }

    #[fuchsia::test]
    async fn test_usb_watcher() -> Result<()> {
        let empty_signal = Arc::new(Mutex::new(false));
        let serial_finder = TestSerialNumberFinder {
            responses: vec![
                vec!["1234".to_string(), "2345".to_string(), "ABCD".to_string()],
                vec!["1234".to_string(), "5678".to_string()],
            ],
            is_empty: empty_signal.clone(),
        };

        let mut serial_to_is_fastboot = HashMap::new();
        serial_to_is_fastboot.insert("1234".to_string(), true);
        serial_to_is_fastboot.insert("2345".to_string(), true);
        serial_to_is_fastboot.insert("5678".to_string(), true);
        // Since this is not in fastboot then it should not appear in our results
        serial_to_is_fastboot.insert("ABCD".to_string(), false);
        let fastboot_tester = TestFastbootUsbTester { serial_to_is_fastboot };

        let mut serial_to_is_live = HashMap::new();
        serial_to_is_live.insert("1234".to_string(), true);
        serial_to_is_live.insert("2345".to_string(), true);
        serial_to_is_live.insert("5678".to_string(), true);

        let (sender, mut queue) = unbounded();
        let watcher = FastbootUsbWatcher::new(
            move |res: Result<FastbootEvent>| {
                let _ = sender.unbounded_send(res);
            },
            serial_finder,
            fastboot_tester,
            Duration::from_millis(1),
        );

        while !*empty_signal.lock().unwrap() {
            // Wait a tiny bit so the watcher can drain the finder queue
            Timer::new(Duration::from_millis(1)).await;
        }

        drop(watcher);
        let mut events = Vec::<FastbootEvent>::new();
        while let Ok(Some(event)) = queue.try_next() {
            events.push(event.unwrap());
        }

        // Assert state of events
        assert_eq!(events.len(), 6);
        assert_eq!(
            &events,
            &vec![
                // First set of discovery events
                FastbootEvent::Discovered("1234".to_string()),
                FastbootEvent::Discovered("5678".to_string()),
                // Second set of discovery events
                FastbootEvent::Discovered("2345".to_string()),
                FastbootEvent::Lost("5678".to_string()),
                // Last set... there are no more items left in the queue
                // so we lose all serials.
                FastbootEvent::Lost("1234".to_string()),
                FastbootEvent::Lost("2345".to_string()),
            ]
        );
        // Reiterating... serial ABCD was not in fastboot so it should not appear in our results
        Ok(())
    }

    struct StackedFastbootUsbLiveTester {
        is_live_queue: VecDeque<bool>,
        call_count: u32,
    }

    impl FastbootUsbLiveTester for StackedFastbootUsbLiveTester {
        async fn is_fastboot_usb_live(&mut self, _serial: &str) -> bool {
            self.call_count += 1;
            self.is_live_queue.pop_front().expect("should have enough calls in the queue")
        }
    }

    #[fuchsia::test]
    async fn test_wait_for_live() -> Result<()> {
        let mut tester = StackedFastbootUsbLiveTester {
            is_live_queue: VecDeque::from([false, false, false, true]),
            call_count: 0,
        };

        wait_for_live("some-awesome-serial", &mut tester, Duration::from_millis(10)).await;

        assert_eq!(tester.call_count, 4);

        Ok(())
    }
}
