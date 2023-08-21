// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    fs::FdEvents,
    task::{EventHandler, WaitCanceler, Waiter},
    types::{error, Errno},
};

use fuchsia_zircon as zx;
use std::sync::Arc;
use syncio::{zxio, Zxio, ZxioSignals};

fn get_zxio_signals_from_events(events: FdEvents) -> zxio::zxio_signals_t {
    let mut signals = ZxioSignals::NONE;

    if events.contains(FdEvents::POLLIN) {
        signals |= ZxioSignals::READABLE;
    }
    if events.contains(FdEvents::POLLPRI) {
        signals |= ZxioSignals::OUT_OF_BAND;
    }
    if events.contains(FdEvents::POLLOUT) {
        signals |= ZxioSignals::WRITABLE;
    }
    if events.contains(FdEvents::POLLERR) {
        signals |= ZxioSignals::ERROR;
    }
    if events.contains(FdEvents::POLLHUP) {
        signals |= ZxioSignals::PEER_CLOSED;
    }
    if events.contains(FdEvents::POLLRDHUP) {
        signals |= ZxioSignals::READ_DISABLED;
    }

    signals.bits()
}

fn get_events_from_zxio_signals(signals: zxio::zxio_signals_t) -> FdEvents {
    let zxio_signals = ZxioSignals::from_bits_truncate(signals);

    let mut events = FdEvents::empty();

    if zxio_signals.contains(ZxioSignals::READABLE) {
        events |= FdEvents::POLLIN;
    }
    if zxio_signals.contains(ZxioSignals::OUT_OF_BAND) {
        events |= FdEvents::POLLPRI;
    }
    if zxio_signals.contains(ZxioSignals::WRITABLE) {
        events |= FdEvents::POLLOUT;
    }
    if zxio_signals.contains(ZxioSignals::ERROR) {
        events |= FdEvents::POLLERR;
    }
    if zxio_signals.contains(ZxioSignals::PEER_CLOSED) {
        events |= FdEvents::POLLHUP;
    }
    if zxio_signals.contains(ZxioSignals::READ_DISABLED) {
        events |= FdEvents::POLLRDHUP;
    }

    events
}

pub fn zxio_wait_async(
    zxio: &Arc<Zxio>,
    waiter: &Waiter,
    events: FdEvents,
    handler: EventHandler,
) -> WaitCanceler {
    let (handle, signals) = zxio.wait_begin(get_zxio_signals_from_events(events));
    if handle.is_invalid() {
        let observed_zxio_signals = zxio.wait_end(zx::Signals::empty());
        let observed_events = get_events_from_zxio_signals(observed_zxio_signals);
        waiter.wake_immediately(observed_events, handler);
        return WaitCanceler::new(move || false);
    }

    let zxio_clone = zxio.clone();
    let signal_handler = move |signals: zx::Signals| {
        let observed_zxio_signals = zxio_clone.wait_end(signals);
        let observed_events = get_events_from_zxio_signals(observed_zxio_signals);
        handler(observed_events);
    };

    // unwrap OK here as errors are only generated from misuse
    let zxio = Arc::downgrade(zxio);
    let canceler =
        waiter.wake_on_zircon_signals(&handle, signals, Box::new(signal_handler)).unwrap();
    WaitCanceler::new(move || {
        if let Some(zxio) = zxio.upgrade() {
            let (handle, signals) = zxio.wait_begin(ZxioSignals::NONE.bits());
            assert!(!handle.is_invalid());
            let did_cancel = canceler.cancel(handle);
            zxio.wait_end(signals);
            did_cancel
        } else {
            false
        }
    })
}

pub fn zxio_query_events(zxio: &Arc<Zxio>) -> Result<FdEvents, Errno> {
    let (handle, signals) = zxio.wait_begin(ZxioSignals::all().bits());
    let observed_signals = if handle.is_invalid() {
        zx::Signals::empty()
    } else {
        match handle.wait(signals, zx::Time::INFINITE_PAST) {
            Ok(signals) => signals,
            Err(zx::Status::TIMED_OUT) => zx::Signals::empty(),
            Err(e) => return error!(EIO, e),
        }
    };
    let observed_zxio_signals = zxio.wait_end(observed_signals);
    Ok(get_events_from_zxio_signals(observed_zxio_signals))
}
