// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Type-safe bindings for Zircon interrupts.

use crate::{
    ok, sys, AsHandleRef, BootTimeline, Handle, HandleBased, HandleRef, Instant, MonotonicTimeline,
    Port, Status, Timeline,
};
use std::marker::PhantomData;

/// An object representing a Zircon interrupt.
///
/// As essentially a subtype of `Handle`, it can be freely interconverted.
#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[repr(transparent)]
pub struct Interrupt<K = RealInterruptKind, T = BootTimeline>(Handle, PhantomData<(K, T)>);

pub trait InterruptKind: private::Sealed {}

#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct VirtualInterruptKind;

impl InterruptKind for VirtualInterruptKind {}
impl private::Sealed for VirtualInterruptKind {}

#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct RealInterruptKind;

impl InterruptKind for RealInterruptKind {}
impl private::Sealed for RealInterruptKind {}

pub type VirtualInterrupt = Interrupt<VirtualInterruptKind>;

impl<K: InterruptKind, T: Timeline> Interrupt<K, T> {
    /// Bind the given port with the given key.
    ///
    /// Wraps [zx_interrupt_bind](https://fuchsia.dev/reference/syscalls/interrupt_bind).
    pub fn bind_port(&self, port: &Port, key: u64) -> Result<(), Status> {
        let options = sys::ZX_INTERRUPT_BIND;
        // SAFETY: This is a basic FFI call.
        let status =
            unsafe { sys::zx_interrupt_bind(self.raw_handle(), port.raw_handle(), key, options) };
        ok(status)
    }

    /// Acknowledge the interrupt.
    ///
    /// Wraps [zx_interrupt_ack](https://fuchsia.dev/reference/syscalls/interrupt_ack).
    pub fn ack(&self) -> Result<(), Status> {
        // SAFETY: This is a basic FFI call.
        let status = unsafe { sys::zx_interrupt_ack(self.raw_handle()) };
        ok(status)
    }
}

pub trait InterruptTimeline: Timeline {
    const CREATE_FLAGS: u32;
}

impl InterruptTimeline for MonotonicTimeline {
    const CREATE_FLAGS: u32 = sys::ZX_INTERRUPT_TIMESTAMP_MONO;
}

impl InterruptTimeline for BootTimeline {
    const CREATE_FLAGS: u32 = 0;
}

impl<T: InterruptTimeline> Interrupt<VirtualInterruptKind, T> {
    /// Create a virtual interrupt.
    ///
    /// Wraps [zx_interrupt_create](https://fuchsia.dev/reference/syscalls/interrupt_create).
    pub fn create_virtual() -> Result<Self, Status> {
        // SAFETY: We are sure that the handle has a valid address.
        let handle = unsafe {
            let mut handle = sys::ZX_HANDLE_INVALID;
            ok(sys::zx_interrupt_create(
                sys::ZX_HANDLE_INVALID,
                T::CREATE_FLAGS,
                sys::ZX_INTERRUPT_VIRTUAL,
                &mut handle,
            ))?;
            Handle::from_raw(handle)
        };
        Ok(Interrupt(handle, PhantomData))
    }

    /// Triggers a virtual interrupt object.
    ///
    /// Wraps [zx_interrupt_trigger](https://fuchsia.dev/reference/syscalls/interrupt_trigger).
    pub fn trigger(&self, time: Instant<T>) -> Result<(), Status> {
        // SAFETY: this is a basic FFI call.
        let status = unsafe { sys::zx_interrupt_trigger(self.raw_handle(), 0, time.into_nanos()) };
        ok(status)
    }
}

impl<K: InterruptKind, T: Timeline> AsHandleRef for Interrupt<K, T> {
    fn as_handle_ref(&self) -> HandleRef<'_> {
        self.0.as_handle_ref()
    }
}

impl<K: InterruptKind, T: Timeline> From<Handle> for Interrupt<K, T> {
    fn from(handle: Handle) -> Self {
        Interrupt::<K, T>(handle, PhantomData)
    }
}

impl<K: InterruptKind, T: Timeline> From<Interrupt<K, T>> for Handle {
    fn from(x: Interrupt<K, T>) -> Handle {
        x.0
    }
}

impl<K: InterruptKind> HandleBased for Interrupt<K> {}

mod private {
    pub trait Sealed {}
}

#[cfg(test)]
mod tests {
    use crate::{
        BootInstant, Handle, Interrupt, MonotonicInstant, MonotonicTimeline, Port, PortOptions,
        Status, VirtualInterrupt, VirtualInterruptKind,
    };

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

    #[test]
    fn trigger() {
        let interrupt = VirtualInterrupt::create_virtual().unwrap();
        let result = interrupt.trigger(BootInstant::from_nanos(10));
        assert_eq!(result, Ok(()));
    }

    #[test]
    fn trigger_monotimeline() {
        let interrupt =
            Interrupt::<VirtualInterruptKind, MonotonicTimeline>::create_virtual().unwrap();
        let result = interrupt.trigger(MonotonicInstant::from_nanos(10));
        assert_eq!(result, Ok(()));
    }
}
