// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Type-safe bindings for Zircon interrupts.

use crate::{ok, sys, AsHandleRef, Handle, HandleBased, HandleRef, Port, Status};

/// An object representing a Zircon interrupt.
///
/// As essentially a subtype of `Handle`, it can be freely interconverted.
#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[repr(transparent)]
pub struct Interrupt(Handle);
impl_handle_based!(Interrupt);

impl Interrupt {
    // Bind the given port with the given key.
    //
    // Wraps [zx_interrupt_bind](https://fuchsia.dev/reference/syscalls/interrupt_bind).
    pub fn bind_port(&self, port: &Port, key: u64) -> Result<(), Status> {
        let options = sys::ZX_INTERRUPT_BIND;
        let status =
            unsafe { sys::zx_interrupt_bind(self.raw_handle(), port.raw_handle(), key, options) };
        ok(status)
    }

    pub fn ack(&self) -> Result<(), Status> {
        let status = unsafe { sys::zx_interrupt_ack(self.raw_handle()) };
        ok(status)
    }
}

#[cfg(test)]
mod tests {
    use crate::{Handle, Interrupt, Port, PortOptions, Status};

    #[test]
    fn bind() {
        let interrupt: Interrupt = Handle::invalid().into();
        let port = Port::create_with_opts(PortOptions::BIND_TO_INTERRUPT);
        let key = 1;
        let result = interrupt.bind_port(&port, key);
        assert_eq!(result.err(), Some(Status::BAD_HANDLE));
    }

    #[test]
    fn ack() {
        let interrupt: Interrupt = Handle::invalid().into();
        let result = interrupt.ack();
        assert_eq!(result.err(), Some(Status::BAD_HANDLE));
    }
}
