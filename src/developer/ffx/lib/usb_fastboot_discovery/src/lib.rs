// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, bail, Result};
use fastboot::{
    command::{ClientVariable, Command},
    reply::Reply,
    send,
};
use fuchsia_async::{Task, Timer};
use std::collections::BTreeSet;
use std::time::Duration;
use usb_bulk::{AsyncInterface as Interface, InterfaceInfo, Open};

// USB fastboot interface IDs
const FASTBOOT_AND_CDC_ETH_USB_DEV_PRODUCT: u16 = 0xa027;
const FASTBOOT_USB_INTERFACE_CLASS: u8 = 0xff;
const FASTBOOT_USB_INTERFACE_SUBCLASS: u8 = 0x42;
const FASTBOOT_USB_INTERFACE_PROTOCOL: u8 = 0x03;

//TODO(fxbug.dev/52733) - serial info will probably get rolled into the target struct

fn is_fastboot_match(info: &InterfaceInfo) -> bool {
    (info.dev_vendor == 0x18d1)
        && ((info.dev_product == 0x4ee0)
            || (info.dev_product == 0x0d02)
            || (info.dev_product == FASTBOOT_AND_CDC_ETH_USB_DEV_PRODUCT))
        && (info.ifc_class == FASTBOOT_USB_INTERFACE_CLASS)
        && (info.ifc_subclass == FASTBOOT_USB_INTERFACE_SUBCLASS)
        && (info.ifc_protocol == FASTBOOT_USB_INTERFACE_PROTOCOL)
}

fn enumerate_interfaces<F>(mut cb: F)
where
    F: FnMut(&InterfaceInfo),
{
    tracing::debug!("Enumerating USB fastboot interfaces");
    let mut cb = |info: &InterfaceInfo| -> bool {
        if is_fastboot_match(info) {
            cb(info)
        }
        // Do not open anything.
        false
    };
    let _result = Interface::open(&mut cb);
}

pub fn find_serial_numbers() -> Vec<String> {
    let mut serials = Vec::new();
    let cb = |info: &InterfaceInfo| serials.push(extract_serial_number(info));
    enumerate_interfaces(cb);
    serials
}

fn open_interface<F>(mut cb: F) -> Result<Interface>
where
    F: FnMut(&InterfaceInfo) -> bool,
{
    tracing::debug!("Selecting USB fastboot interface to open");

    let mut open_cb = |info: &InterfaceInfo| -> bool {
        if is_fastboot_match(info) {
            cb(info)
        } else {
            // Do not open.
            false
        }
    };
    Interface::open(&mut open_cb).map_err(Into::into)
}

fn extract_serial_number(info: &InterfaceInfo) -> String {
    let null_pos = match info.serial_number.iter().position(|&c| c == 0) {
        Some(p) => p,
        None => {
            return "".to_string();
        }
    };
    (*String::from_utf8_lossy(&info.serial_number[..null_pos])).to_string()
}

#[tracing::instrument]
pub async fn open_interface_with_serial(serial: &str) -> Result<Interface> {
    tracing::debug!("Opening USB fastboot interface with serial number: {}", serial);
    let mut interface =
        open_interface(|info: &InterfaceInfo| -> bool { extract_serial_number(info) == *serial })?;
    if let Ok(Reply::Okay(version)) =
        send(Command::GetVar(ClientVariable::Version), &mut interface).await
    {
        // Only support 0.4 right now.
        if version == "0.4".to_string() {
            Ok(interface)
        } else {
            bail!(format!("USB serial {serial}: wrong version ({version})"))
        }
    } else {
        bail!(format!("USB serial {serial}: could not get version"))
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

pub trait FastbootEventHandler: Send + 'static {
    /// Handles an event.
    fn handle_event(&mut self, event: Result<FastbootEvent>);
}

impl<F> FastbootEventHandler for F
where
    F: FnMut(Result<FastbootEvent>) -> () + Send + 'static,
{
    fn handle_event(&mut self, x: Result<FastbootEvent>) -> () {
        self(x)
    }
}

trait SerialNumberFinder: Send + 'static {
    fn find_serial_numbers(&mut self) -> Vec<String>;
}

impl<F> SerialNumberFinder for F
where
    F: FnMut() -> Vec<String> + Send + 'static,
{
    fn find_serial_numbers(&mut self) -> Vec<String> {
        self()
    }
}

pub fn recommended_watcher<F>(event_handler: F) -> Result<FastbootUsbWatcher>
where
    F: FastbootEventHandler,
{
    Ok(FastbootUsbWatcher::new(event_handler, find_serial_numbers, Duration::from_secs(1)))
}

impl FastbootUsbWatcher {
    fn new<F, W>(event_handler: F, finder: W, interval: Duration) -> Self
    where
        F: FastbootEventHandler,
        W: SerialNumberFinder,
    {
        let mut res = Self { discovery_task: None, drain_task: None };

        let (sender, receiver) = async_channel::bounded::<FastbootEvent>(1);

        res.discovery_task.replace(Task::local(discovery_loop(sender, finder, interval)));
        res.drain_task.replace(Task::local(handle_events_loop(receiver, event_handler)));

        res
    }
}

async fn discovery_loop<F>(
    events_out: async_channel::Sender<FastbootEvent>,
    mut finder: F,
    discovery_interval: Duration,
) -> ()
where
    F: SerialNumberFinder,
{
    let mut serials = BTreeSet::<String>::new();
    loop {
        // Enumerate interfaces
        let new_serials = finder.find_serial_numbers();
        let new_serials = BTreeSet::from_iter(new_serials);
        tracing::trace!("found serials: {:#?}", new_serials);
        // Update Cache
        for serial in &new_serials {
            tracing::trace!("Inserting new serial: {}", serial);
            if serials.insert(serial.clone()) {
                tracing::trace!("Sending discovered event for serial: {}", serial);
                let _ = events_out.send(FastbootEvent::Discovered(serial.clone())).await;
                tracing::trace!("Sent discovered event for serial: {}", serial);
            }
        }

        // Check for any missing Serials
        let missing_serials: Vec<_> = serials.difference(&new_serials).cloned().collect();
        tracing::trace!("missing serials: {:#?}", missing_serials);
        for serial in missing_serials {
            serials.remove(&serial);
            tracing::trace!("Sening lost event for serial: {}", serial);
            let _ = events_out.send(FastbootEvent::Lost(serial.clone())).await;
            tracing::trace!("Sent lost event for serial: {}", serial);
        }

        tracing::trace!("discovery loop... waiting for {:#?}", discovery_interval);
        Timer::new(discovery_interval).await;
    }
}

async fn handle_events_loop<F>(receiver: async_channel::Receiver<FastbootEvent>, mut handler: F)
where
    F: FastbootEventHandler,
{
    loop {
        let event = receiver.recv().await.map_err(|e| anyhow!(e));
        tracing::trace!("Event loop received event: {:#?}", event);
        handler.handle_event(event);
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use futures::channel::mpsc::unbounded;
    use pretty_assertions::assert_eq;
    use std::sync::{Arc, Mutex};

    struct TestSerialNumberFinder {
        responses: Vec<Vec<String>>,
        is_empty: Arc<Mutex<bool>>,
    }

    impl SerialNumberFinder for TestSerialNumberFinder {
        fn find_serial_numbers(&mut self) -> Vec<String> {
            if let Some(res) = self.responses.pop() {
                res
            } else {
                let mut lock = self.is_empty.lock().unwrap();
                *lock = true;
                vec![]
            }
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_usb_watcher() -> Result<()> {
        let empty_signal = Arc::new(Mutex::new(false));
        let serial_finder = TestSerialNumberFinder {
            responses: vec![
                vec!["1234".to_string(), "2345".to_string()],
                vec!["1234".to_string(), "5678".to_string()],
            ],
            is_empty: empty_signal.clone(),
        };
        let (sender, mut queue) = unbounded();
        let watcher = FastbootUsbWatcher::new(
            move |res: Result<FastbootEvent>| {
                let _ = sender.unbounded_send(res);
            },
            serial_finder,
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
        Ok(())
    }
}
