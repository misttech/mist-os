// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style licence that can be
// found in the LICENSE file.

use anyhow::{Context, Result};
use async_trait::async_trait;
use ffx_fastboot_interface::interface_factory::{
    InterfaceFactory, InterfaceFactoryBase, InterfaceFactoryError,
};
use fuchsia_async::MonotonicDuration;
use futures::channel::oneshot::{channel, Sender};
use usb_bulk::AsyncInterface;
use usb_fastboot_discovery::{
    find_serial_numbers, open_interface_with_serial, wait_for_live, FastbootEvent,
    FastbootEventHandler, FastbootUsbLiveTester, FastbootUsbWatcher, UnversionedFastbootUsbTester,
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

struct UsbTargetHandler {
    // The serial number we are looking for
    target_serial: String,
    // Channel to send on once we find the usb device with the provided serial
    // number is up and ready to accept fastboot commands
    tx: Option<Sender<()>>,
}

impl FastbootEventHandler for UsbTargetHandler {
    async fn handle_event(&mut self, event: Result<FastbootEvent>) {
        if self.tx.is_none() {
            tracing::warn!("Handling an event but our sender is none. Returning early.");
            return;
        }
        match event {
            Ok(ev) => match ev {
                FastbootEvent::Discovered(s) if s == self.target_serial => {
                    tracing::debug!(
                        "Discovered target with serial: {} we were looking for!",
                        self.target_serial
                    );
                    let _ = self.tx.take().unwrap().send(());
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
            },
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

pub struct StrictGetVarFastbootUsbLiveTester {
    serial: String,
}

impl FastbootUsbLiveTester for StrictGetVarFastbootUsbLiveTester {
    async fn is_fastboot_usb_live(&mut self, serial: &str) -> bool {
        *serial == self.serial && open_interface_with_serial(serial).await.is_ok()
    }
}

#[async_trait(?Send)]
impl InterfaceFactoryBase<AsyncInterface> for UsbFactory {
    async fn open(&mut self) -> Result<AsyncInterface, InterfaceFactoryError> {
        let interface = open_interface_with_serial(&self.serial).await.with_context(|| {
            format!("Failed to open target usb interface by serial {}", self.serial)
        })?;
        tracing::debug!("serial now in use: {}", self.serial);
        Ok(interface)
    }

    async fn close(&self) {
        tracing::debug!("dropping UsbFactory for serial: {}", self.serial);
    }

    async fn rediscover(&mut self) -> Result<(), InterfaceFactoryError> {
        tracing::debug!("Rediscovering devices");

        let (tx, rx) = channel::<()>();
        // Handler will handle the found usb devices.
        // Will filter to only usb devices that match the given serial number
        // Will send a signal once both the serial number is found _and_
        // is readily accepting fastboot commands
        let handler = UsbTargetHandler { tx: Some(tx), target_serial: self.serial.clone() };

        // This is usb therefore we only need to find usb targets
        let watcher = FastbootUsbWatcher::new(
            handler,
            find_serial_numbers,
            // This tester will not attempt to talk to the USB devices to extract version info, it
            // only inspects the USB interface
            UnversionedFastbootUsbTester {},
            MonotonicDuration::from_secs(1),
        );

        rx.await.map_err(|e| {
            anyhow::anyhow!("error awaiting oneshot channel rediscovering target: {}", e)
        })?;

        drop(watcher);

        let mut tester = StrictGetVarFastbootUsbLiveTester { serial: self.serial.clone() };
        tracing::debug!(
            "Rediscovered device with serial {}. Waiting for it to be live",
            self.serial,
        );
        wait_for_live(self.serial.as_str(), &mut tester, MonotonicDuration::from_millis(500)).await;

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

    ///////////////////////////////////////////////////////////////////////////////
    // UsbTargetHandler
    //

    #[fuchsia::test]
    async fn handle_target_test() -> Result<()> {
        let target_serial = "1234567890".to_string();

        let (tx, mut rx) = channel::<()>();
        let mut handler = UsbTargetHandler { tx: Some(tx), target_serial: target_serial.clone() };

        //Lost our serial
        handler.handle_event(Ok(FastbootEvent::Lost(target_serial.clone()))).await;
        assert!(rx.try_recv().unwrap().is_none());
        // Lost a different serial
        handler.handle_event(Ok(FastbootEvent::Lost("1234asdf".to_string()))).await;
        assert!(rx.try_recv().unwrap().is_none());
        // Found a new serial
        handler.handle_event(Ok(FastbootEvent::Discovered("1234asdf".to_string()))).await;
        assert!(rx.try_recv().unwrap().is_none());
        // Error
        handler.handle_event(Err(anyhow!("Hello there friends"))).await;
        assert!(rx.try_recv().unwrap().is_none());
        // Found our serial
        handler.handle_event(Ok(FastbootEvent::Discovered(target_serial.clone()))).await;
        assert!(rx.try_recv().unwrap().is_some());

        Ok(())
    }
}
