// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Result};
use async_trait::async_trait;
use ffx_fastboot_interface::interface_factory::{InterfaceFactory, InterfaceFactoryBase};
use fuchsia_async::Duration;
use futures::channel::oneshot::{channel, Sender};
use usb_bulk::AsyncInterface;
use usb_fastboot_discovery::{
    find_serial_numbers, open_interface_with_serial, FastbootEvent, FastbootEventHandler,
    FastbootUsbWatcher, UnversionedFastbootUsbTester,
};

///////////////////////////////////////////////////////////////////////////////
// UsbFactory
//

#[derive(Default, Debug, Clone)]
pub struct UsbFactory {
    serial: String,
}

impl UsbFactory {
    pub fn new(serial: String) -> Self {
        Self { serial }
    }
}

trait UsbLiveTester: Send + Clone {
    async fn is_usb_live(self, serial: &String) -> bool;
}

#[derive(Clone)]
struct OpenInterfaceLiveTester;

impl UsbLiveTester for OpenInterfaceLiveTester {
    async fn is_usb_live(self, serial: &String) -> bool {
        open_interface_with_serial(serial.as_str()).await.is_ok()
    }
}

struct UsbTargetHandler<T>
where
    T: UsbLiveTester,
{
    // The serial number we are looking for
    target_serial: String,
    // Channel to send on once we find the usb device with the provided serial
    // number is up and ready to accept fastboot commands
    tx: Option<Sender<()>>,
    // Something to test if the usb interface is ready to accept fastboot commands.
    // Cannot use the OpenInterfaceLiveTester in tests as it accesses the system
    // need to stub it out for testing
    test: T,
}

impl<T> FastbootEventHandler for UsbTargetHandler<T>
where
    T: UsbLiveTester + 'static,
{
    fn handle_event(&mut self, event: Result<FastbootEvent>) {
        if self.tx.is_none() {
            tracing::warn!("Handling an event but our sender is none. Returning early.");
            return;
        }
        match event {
            Ok(ev) => {
                match ev {
                    FastbootEvent::Discovered(s) if s == self.target_serial => {
                        let ts_clone = self.target_serial.clone();
                        // Test if it is live...  if it isn't, then return
                        let tester = self.test.clone();
                        let res = futures::executor::block_on(async move {
                            tester.is_usb_live(&ts_clone).await
                        });
                        if res == true {
                            tracing::debug!("USB Device with serial {} is ready to respond to fastboot commands", self.target_serial);
                            let _ = self.tx.take().unwrap().send(());
                        } else {
                            tracing::debug!(
                            "USB Device with serial {} is not yet ready to respond to fastboot commands",
                            self.target_serial
                        );
                        }
                    }
                    FastbootEvent::Discovered(s) => tracing::debug!(
                        "Attempting to rediscover target with serial: {}. Found target: {}",
                        self.target_serial,
                        s,
                    ),
                    FastbootEvent::Lost(l) => tracing::debug!(
                        "Attempting to rediscover target with serial: {}. Lost target: {}",
                        self.target_serial,
                        l,
                    ),
                }
            }
            Err(e) => {
                tracing::error!(
                    "Attempting to rediscover target with serial: {}. Got an error: {}. Continuing",
                    self.target_serial,
                    e
                )
            }
        }
    }
}

#[async_trait(?Send)]
impl InterfaceFactoryBase<AsyncInterface> for UsbFactory {
    async fn open(&mut self) -> Result<AsyncInterface> {
        let interface = open_interface_with_serial(&self.serial).await.with_context(|| {
            format!("Failed to open target usb interface by serial {}", self.serial)
        })?;
        tracing::debug!("serial now in use: {}", self.serial);
        Ok(interface)
    }

    async fn close(&self) {
        tracing::debug!("dropping UsbFactory for serial: {}", self.serial);
    }

    async fn rediscover(&mut self) -> Result<()> {
        tracing::debug!("Rediscovering devices");

        let (tx, rx) = channel::<()>();
        // Handler will handle the found usb devices.
        // Will filter to only usb devices that match the given serial number
        // Will send a signal once both the serial number is found _and_
        // is readily accepting fastboot commands
        let handler = UsbTargetHandler {
            tx: Some(tx),
            target_serial: self.serial.clone(),
            test: OpenInterfaceLiveTester {},
        };

        // This is usb therefore we only need to find usb targets
        let watcher = FastbootUsbWatcher::new(
            handler,
            find_serial_numbers,
            // This tester will not attempt to talk to the USB devices to extract version info, it
            // only inspects the USB interface
            UnversionedFastbootUsbTester {},
            Duration::from_secs(1),
        );

        rx.await?;

        drop(watcher);
        Ok(())
    }
}

impl Drop for UsbFactory {
    fn drop(&mut self) {
        futures::executor::block_on(async move {
            self.close().await;
        });
    }
}

impl InterfaceFactory<AsyncInterface> for UsbFactory {}

#[cfg(test)]
mod test {
    use super::*;
    use anyhow::anyhow;
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct DummyInterfaceLiveTester {
        serial_number: String,
        call_stack: Arc<Mutex<Vec<bool>>>,
    }

    impl UsbLiveTester for DummyInterfaceLiveTester {
        async fn is_usb_live(self, serial: &String) -> bool {
            if serial != &self.serial_number {
                panic!("We should never be asked for a different serial number");
            }
            let mut stack = self.call_stack.lock().unwrap();
            match stack.pop() {
                None => {
                    panic!("No calls left in the stack")
                }
                Some(x) => x,
            }
        }
    }

    ///////////////////////////////////////////////////////////////////////////////
    // UsbTargetHandler
    //

    #[test]
    fn handle_target_test() -> Result<()> {
        let target_serial = "1234567890".to_string();

        let (tx, mut rx) = channel::<()>();
        let call_stack = Arc::new(Mutex::new(vec![true, false, false]));
        let tester = DummyInterfaceLiveTester { serial_number: target_serial.clone(), call_stack };
        let mut handler =
            UsbTargetHandler { tx: Some(tx), target_serial: target_serial.clone(), test: tester };

        //Lost our serial
        handler.handle_event(Ok(FastbootEvent::Lost(target_serial.clone())));
        assert!(rx.try_recv().unwrap().is_none());
        // Lost a different serial
        handler.handle_event(Ok(FastbootEvent::Lost("1234asdf".to_string())));
        assert!(rx.try_recv().unwrap().is_none());
        // Found a new serial
        handler.handle_event(Ok(FastbootEvent::Discovered("1234asdf".to_string())));
        assert!(rx.try_recv().unwrap().is_none());
        // Error
        handler.handle_event(Err(anyhow!("Hello there friends")));
        assert!(rx.try_recv().unwrap().is_none());
        // Found our serial, but is not ready
        handler.handle_event(Ok(FastbootEvent::Discovered(target_serial.clone())));
        assert!(rx.try_recv().unwrap().is_none());
        // Found our serial, but is STILL not ready
        handler.handle_event(Ok(FastbootEvent::Discovered(target_serial.clone())));
        assert!(rx.try_recv().unwrap().is_none());
        // Found our serial and is ready
        handler.handle_event(Ok(FastbootEvent::Discovered(target_serial.clone())));
        assert!(rx.try_recv().unwrap().is_some());

        Ok(())
    }
}
