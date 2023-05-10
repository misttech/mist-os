// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{format_err, Error};
use bind_fuchsia_google_usb::{
    BIND_USB_PROTOCOL_OVERNET, BIND_USB_SUBCLASS_OVERNET, BIND_USB_VID_GOOGLE,
};
use bind_fuchsia_usb::BIND_USB_CLASS_VENDOR_SPECIFIC;
use futures::future::{select, try_join, Either, FutureExt};
use futures::stream::StreamExt;
use overnet_core::Router;
use std::sync::Weak;

static OVERNET_MAGIC: &[u8; 16] = b"OVERNET USB\xff\x00\xff\x00\xff";
const MAGIC_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const MTU: usize = 1024;

pub async fn listen_for_usb_devices(router: Weak<Router>) -> Result<(), Error> {
    tracing::info!("Listening for USB devices");
    let mut stream = usb_rs::wait_for_devices(true, false)?;
    while let Some(device) = stream.next().await.transpose()? {
        let usb_rs::DeviceEvent::Added(device) = device  else {
            continue;
        };

        // We may soon replace Overnet entirely with a new protocol called FDomain. If we do we'll
        // start exposing a different USB interface. This tracks whether we've seen a USB interface
        // that's similar to what we're expecting but with a higher protocol number. If so, we'll
        // warn the user that they might be too out of date to talk to their device.
        let saw_potential_fdomain = std::sync::atomic::AtomicBool::new(false);

        let interface = match device.scan_interfaces(|device, interface| {
            let subclass_match = u32::from(device.vendor) == BIND_USB_VID_GOOGLE
                && u32::from(interface.class) == BIND_USB_CLASS_VENDOR_SPECIFIC
                && u32::from(interface.subclass) == BIND_USB_SUBCLASS_OVERNET;
            let protocol_match = u32::from(interface.protocol) == BIND_USB_PROTOCOL_OVERNET;
            if subclass_match && !protocol_match {
                saw_potential_fdomain.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            subclass_match && protocol_match
        }) {
            Ok(dev) => dev,
            Err(usb_rs::Error::InterfaceNotFound) => {
                if saw_potential_fdomain.into_inner() {
                    tracing::warn!(
                        device = device.debug_name().as_str(),
                        "Device may be serving a much newer ffx protocol. \
                     Consider updating ffx if you expect to communicate with this device."
                    );
                }
                continue;
            }
            Err(e) => {
                tracing::warn!(device = device.debug_name().as_str(), error = ?e, "Error scanning USB device");
                continue;
            }
        };

        router.upgrade().ok_or_else(|| format_err!("Router gone"))?;
        if let Err(e) = run_usb_link(device.debug_name(), interface, router.clone()).await {
            tracing::warn!("USB link terminated with error: {:?}", e)
        } else {
            tracing::info!("Shut down USB link for {}", device.debug_name())
        }
    }

    Ok(())
}

async fn run_usb_link(
    device: String,
    interface: usb_rs::Interface,
    router: Weak<Router>,
) -> Result<(), Error> {
    tracing::info!("Setting up USB link for {device}");
    let debug_path = device;

    let mut in_ep = None;
    let mut out_ep = None;

    for endpoint in interface.endpoints() {
        match endpoint {
            usb_rs::Endpoint::BulkIn(endpoint) => {
                if in_ep.is_some() {
                    tracing::warn!(
                        device = debug_path.as_str(),
                        "Multiple bulk in endpoints on interface"
                    );
                } else {
                    in_ep = Some(endpoint)
                }
            }
            usb_rs::Endpoint::BulkOut(endpoint) => {
                if out_ep.is_some() {
                    tracing::warn!(
                        device = debug_path.as_str(),
                        "Multiple bulk out endpoints on interface"
                    );
                } else {
                    out_ep = Some(endpoint)
                }
            }
            _ => (),
        }
    }

    let in_ep = in_ep.ok_or_else(|| format_err!("In endpoint missing"))?;
    let out_ep = out_ep.ok_or_else(|| format_err!("Out endpoint missing"))?;

    out_ep.write(OVERNET_MAGIC).await?;

    let mut magic_timer = fuchsia_async::Timer::new(MAGIC_TIMEOUT);
    let mut buf = [0u8; MTU];
    loop {
        let size = {
            let read_fut = in_ep.read(&mut buf);
            futures::pin_mut!(read_fut);
            match select(read_fut, &mut magic_timer).await {
                Either::Left((got, _)) => got?,
                Either::Right((_, fut)) => {
                    if let Some(got) = fut.now_or_never() {
                        got?
                    } else {
                        return Err(format_err!("Timed out waiting for driver to synchronize"));
                    }
                }
            }
        };

        let magic = <&[u8; 16]>::try_from(&buf[..size]).ok();

        if magic.filter(|x| **x == *OVERNET_MAGIC).is_some() {
            break;
        }

        tracing::warn!(device = debug_path.as_str(), "Discarding {size} unexpected bytes");
    }

    let (out_ep_reader, out_writer) = circuit::stream::stream();
    let (in_reader, in_ep_writer) = circuit::stream::stream();
    let (error_sender, mut error_receiver) = futures::channel::mpsc::unbounded();

    let router = router.upgrade().ok_or_else(|| format_err!("Router gone"))?;

    let conn = circuit::multi_stream::multi_stream_node_connection(
        router.circuit_node(),
        in_reader,
        out_writer,
        false,
        circuit::Quality::USB,
        error_sender,
        format!("USB device {debug_path}"),
    );

    let error_logger = {
        let debug_path = debug_path.clone();
        async move {
            while let Some(error) = error_receiver.next().await {
                tracing::debug!(
                    usb_device = debug_path.as_str(),
                    ?error,
                    "Stream encountered an error"
                );
            }
        }
    };

    let conn = async move {
        futures::pin_mut!(conn);
        futures::pin_mut!(error_logger);

        match select(conn, error_logger).await {
            Either::Left((e, _)) => {
                e?;
            }
            Either::Right(((), conn)) => conn.await?,
        }
        Ok(())
    };

    tracing::debug!(usb_device = debug_path.as_str(), "Established USB link");

    let tx = async move {
        loop {
            // TODO(sadmac): Change to MTU when fxb/124327 is fixed
            let mut out = [0u8; 496];

            // TODO: We could save a copy here by having a version of `read` with an async body.
            let got = out_ep_reader
                .read(1, |buf| {
                    let len = std::cmp::min(buf.len(), out.len());
                    out[..len].copy_from_slice(&buf[..len]);
                    Ok((len, len))
                })
                .await?;

            out_ep.write(&out[..got]).await?;
        }
    };

    let rx = async move {
        let mut data = [0; 2048];
        loop {
            let size = in_ep.read(&mut data).await?;

            if size == 0 {
                continue;
            }

            in_ep_writer.write(size, |buf| {
                buf[..size].copy_from_slice(&data[..size]);
                Ok(size)
            })?;
        }
    };

    let tx_rx = async move {
        futures::pin_mut!(tx);
        futures::pin_mut!(rx);
        match select(tx, rx).await {
            Either::Left((e, _)) => {
                if let Result::<(), Error>::Err(e) = e {
                    tracing::warn!(usb_device = debug_path.as_str(), "Transmit failed: {:?}", e);
                    Err(e)
                } else {
                    tracing::debug!(usb_device = debug_path.as_str(), "Transmit closed");
                    Ok(())
                }
            }
            Either::Right((e, _)) => {
                if let Result::<(), Error>::Err(e) = e {
                    tracing::warn!(usb_device = debug_path.as_str(), "Receive failed: {:?}", e);
                    Err(e)
                } else {
                    tracing::debug!(usb_device = debug_path.as_str(), "Receive closed");
                    Ok(())
                }
            }
        }
    };

    try_join(tx_rx, conn).await.map(std::mem::drop)
}
