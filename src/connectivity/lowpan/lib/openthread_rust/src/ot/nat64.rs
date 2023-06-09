// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::prelude_internal::*;

/// Methods from the [OpenThread "NAT64" Module](https://openthread.io/reference/group/api-nat64).
pub trait Nat64 {
    /// Enable or disable NAT64 functions.
    fn nat64_set_enabled(&self, enabled: bool);

    /// Sets the CIDR used when setting the source address of the outgoing translated IPv4 packets.
    fn nat64_set_ip4_cidr(&self, ip4_cidr: Ip4Cidr) -> Result;

    /// Registers a callback to provide received IPv4 datagrams.
    fn nat64_set_receive_fn<'a, F>(&'a self, f: Option<F>)
    where
        F: FnMut(OtMessageBox<'_>) + 'a;

    /// Send a message to NAT64 handler in OpenThread
    fn nat64_send(&self, message: OtMessageBox<'_>) -> Result;

    /// Send data to NAT64 handler in OpenThread
    fn nat64_send_data_slice(&self, data_slice: &[u8]) -> Result;

    /// Return AIL prefix to OpenThread
    fn nat64_infra_if_prefix_discover_done(
        &self,
        infra_if_idx: ot::NetifIndex,
        ip6_prefix: openthread_sys::otIp6Prefix,
    );
}

impl<T: Nat64 + ot::Boxable> Nat64 for ot::Box<T> {
    fn nat64_set_enabled(&self, enabled: bool) {
        self.as_ref().nat64_set_enabled(enabled)
    }
    fn nat64_set_ip4_cidr(&self, ip4_cidr: Ip4Cidr) -> Result {
        self.as_ref().nat64_set_ip4_cidr(ip4_cidr)
    }
    fn nat64_set_receive_fn<'a, F>(&'a self, f: Option<F>)
    where
        F: FnMut(OtMessageBox<'_>) + 'a,
    {
        self.as_ref().nat64_set_receive_fn(f)
    }
    fn nat64_send(&self, message: OtMessageBox<'_>) -> Result {
        self.as_ref().nat64_send(message)
    }
    fn nat64_send_data_slice(&self, data_slice: &[u8]) -> Result {
        self.as_ref().nat64_send_data_slice(data_slice)
    }
    fn nat64_infra_if_prefix_discover_done(
        &self,
        infra_if_idx: ot::NetifIndex,
        ip6_prefix: openthread_sys::otIp6Prefix,
    ) {
        self.as_ref().nat64_infra_if_prefix_discover_done(infra_if_idx, ip6_prefix);
    }
}

impl Nat64 for Instance {
    fn nat64_set_enabled(&self, enabled: bool) {
        unsafe { otNat64SetEnabled(self.as_ot_ptr(), enabled) }
    }
    fn nat64_set_ip4_cidr(&self, ip4_cidr: Ip4Cidr) -> Result {
        Error::from(unsafe { otNat64SetIp4Cidr(self.as_ot_ptr(), ip4_cidr.as_ot_ptr()) }).into()
    }
    fn nat64_set_receive_fn<'a, F>(&'a self, f: Option<F>)
    where
        F: FnMut(OtMessageBox<'_>) + 'a,
    {
        unsafe extern "C" fn _ot_nat64_receive_callback<'a, F: FnMut(OtMessageBox<'_>) + 'a>(
            message: *mut otMessage,
            context: *mut ::std::os::raw::c_void,
        ) {
            trace!("_ot_nat64_receive_callback");

            // Convert the `*otMessage` into an `ot::Box<ot::Message>`.
            let message = OtMessageBox::from_ot_ptr(message)
                .expect("_ot_nat64_receive_callback: Got NULL otMessage");

            // Reconstitute a reference to our closure.
            let sender = &mut *(context as *mut F);

            sender(message)
        }

        let (fn_ptr, fn_box, cb): (_, _, otNat64ReceiveIp4Callback) = if let Some(f) = f {
            let mut x = Box::new(f);

            (
                x.as_mut() as *mut F as *mut ::std::os::raw::c_void,
                Some(x as Box<dyn FnMut(OtMessageBox<'_>) + 'a>),
                Some(_ot_nat64_receive_callback::<F>),
            )
        } else {
            (std::ptr::null_mut() as *mut ::std::os::raw::c_void, None, None)
        };

        unsafe {
            otNat64SetReceiveIp4Callback(self.as_ot_ptr(), cb, fn_ptr);

            // Make sure our object eventually gets cleaned up.
            // Here we must also transmute our closure to have a 'static lifetime.
            // We need to do this because the borrow checker cannot infer the
            // proper lifetime for the singleton instance backing, but
            // this is guaranteed by the API.
            self.borrow_backing().nat64_receive_fn.set(std::mem::transmute::<
                Option<Box<dyn FnMut(OtMessageBox<'_>) + 'a>>,
                Option<Box<dyn FnMut(OtMessageBox<'_>) + 'static>>,
            >(fn_box));
        }
    }

    fn nat64_send(&self, message: OtMessageBox<'_>) -> Result {
        Error::from(unsafe { otNat64Send(self.as_ot_ptr(), message.take_ot_ptr()) }).into()
    }

    fn nat64_send_data_slice(&self, data_slice: &[u8]) -> Result {
        if let Ok(mut msg) = Message::ip4_new(self, None) {
            msg.append(data_slice)?;
            self.nat64_send(msg)?;
            Ok(())
        } else if self.get_buffer_info().0.mFreeBuffers == 0 {
            Err(ot::Error::NoBufs)
        } else {
            Err(ot::Error::Failed)
        }
    }

    fn nat64_infra_if_prefix_discover_done(
        &self,
        infra_if_idx: ot::NetifIndex,
        ip6_prefix: openthread_sys::otIp6Prefix,
    ) {
        unsafe {
            openthread_sys::otPlatInfraIfDiscoverNat64PrefixDone(
                self.as_ot_ptr(),
                infra_if_idx,
                &ip6_prefix,
            )
        }
    }
}
