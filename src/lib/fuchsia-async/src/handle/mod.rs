// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[cfg(target_os = "fuchsia")]
mod zircon;

#[cfg(target_os = "fuchsia")]
pub use zircon::*;

#[cfg(not(target_os = "fuchsia"))]
mod emulated;

#[cfg(not(target_os = "fuchsia"))]
pub use emulated::*;

/// invoke_for_handle_types!{mmm} calls the macro `mmm!` with two arguments: one is the name of a
/// Zircon handle, the second is one of:
///   * Everywhere for handle types that are supported everywhere FIDL is
///   * FuchsiaOnly for handle types that are supported only on Fuchsia
///   * Stub for handle types that have not yet had a Fuchsia API implemented in the zircon crate
///
/// To make a handle available everywhere, a polyfill must be implemented in
/// crate::handle::emulated.
#[macro_export]
macro_rules! invoke_for_handle_types {
    ($x:ident) => {
        $x! {Process, "Process", PROCESS, ZX_OBJ_TYPE_PROCESS, FuchsiaOnly}
        $x! {Thread, "Thread", THREAD, ZX_OBJ_TYPE_THREAD, FuchsiaOnly}
        $x! {Vmo, "Vmo", VMO, ZX_OBJ_TYPE_VMO, FuchsiaOnly}
        $x! {Channel, "Channel", CHANNEL, ZX_OBJ_TYPE_CHANNEL, Everywhere}
        $x! {Event, "Event", EVENT, ZX_OBJ_TYPE_EVENT, Everywhere}
        $x! {Port, "Port", PORT, ZX_OBJ_TYPE_PORT, FuchsiaOnly}
        $x! {Interrupt, "Interrupt", INTERRUPT, ZX_OBJ_TYPE_INTERRUPT, FuchsiaOnly}
        $x! {DebugLog, "Debug Log", DEBUGLOG, ZX_OBJ_TYPE_DEBUGLOG, FuchsiaOnly}
        $x! {Socket, "Socket", SOCKET, ZX_OBJ_TYPE_SOCKET, Everywhere}
        $x! {Resource, "Resource", RESOURCE, ZX_OBJ_TYPE_RESOURCE, FuchsiaOnly}
        $x! {EventPair, "Event Pair", EVENTPAIR, ZX_OBJ_TYPE_EVENTPAIR, Everywhere}
        $x! {Job, "Job", JOB, ZX_OBJ_TYPE_JOB, FuchsiaOnly}
        $x! {Vmar, "VMAR", VMAR, ZX_OBJ_TYPE_VMAR, FuchsiaOnly}
        $x! {Fifo, "FIFO", FIFO, ZX_OBJ_TYPE_FIFO, FuchsiaOnly}
        $x! {Guest, "Guest", GUEST, ZX_OBJ_TYPE_GUEST, FuchsiaOnly}
        $x! {Vcpu, "VCPU", VCPU, ZX_OBJ_TYPE_VCPU, FuchsiaOnly}
        $x! {Timer, "Timer", TIMER, ZX_OBJ_TYPE_TIMER, FuchsiaOnly}
        $x! {Iob, "Iobuffer", IOB, ZX_OBJ_TYPE_IOB, FuchsiaOnly}
        $x! {Iommu, "IOMMU", IOMMU, ZX_OBJ_TYPE_IOMMU, Stub}
        $x! {Bti, "BTI", BTI, ZX_OBJ_TYPE_BTI, Stub}
        $x! {Profile, "Profile", PROFILE, ZX_OBJ_TYPE_PROFILE, FuchsiaOnly}
        $x! {Pmt, "PMT", PMT, ZX_OBJ_TYPE_PMT, Stub}
        $x! {SuspendToken, "Suspend Token", SUSPEND_TOKEN, ZX_OBJ_TYPE_SUSPEND_TOKEN, Stub}
        $x! {Pager, "Pager", PAGER, ZX_OBJ_TYPE_PAGER, Stub}
        $x! {Exception, "Exception", EXCEPTION, ZX_OBJ_TYPE_EXCEPTION, Stub}
        $x! {Clock, "Clock", CLOCK, ZX_OBJ_TYPE_CLOCK, FuchsiaOnly}
        $x! {Stream, "Stream", STREAM, ZX_OBJ_TYPE_STREAM, FuchsiaOnly}
        $x! {Msi, "MSI", MSI, ZX_OBJ_TYPE_MSI, Stub}
        $x! {PciDevice, "PCI Device", PCI_DEVICE, ZX_OBJ_TYPE_PCI_DEVICE, Stub}
    };
}
