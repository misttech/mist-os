// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Common traits and types for dealing with abstracted frames.

use net_types::{
    ethernet::Mac,
    ip::{Ip, IpVersionMarker},
    BroadcastAddr, MulticastAddr,
};

use core::{convert::Infallible as Never, fmt::Debug};
use packet::{BufferMut, Serializer};

/// A context for receiving frames.
///
/// Note: Use this trait as trait bounds, but always implement
/// [`ReceivableFrameMeta`] instead, which generates a `RecvFrameContext`
/// implementation.
pub trait RecvFrameContext<Meta, BC> {
    /// Receive a frame.
    ///
    /// `receive_frame` receives a frame with the given metadata.
    fn receive_frame<B: BufferMut + Debug>(
        &mut self,
        bindings_ctx: &mut BC,
        metadata: Meta,
        frame: B,
    );
}

impl<CC, BC> ReceivableFrameMeta<CC, BC> for Never {
    fn receive_meta<B: BufferMut + Debug>(
        self,
        _core_ctx: &mut CC,
        _bindings_ctx: &mut BC,
        _frame: B,
    ) {
        match self {}
    }
}

/// A trait providing the receive implementation for some frame identified by a
/// metadata type.
///
/// This trait sidesteps orphan rules by allowing [`RecvFrameContext`] to be
/// implemented by the multiple core crates, given it can always be implemented
/// for a local metadata type. `ReceivableFrameMeta` should always be used for
/// trait implementations, while [`RecvFrameContext`] is used for trait bounds.
pub trait ReceivableFrameMeta<CC, BC> {
    /// Receives this frame using the provided contexts.
    fn receive_meta<B: BufferMut + Debug>(self, core_ctx: &mut CC, bindings_ctx: &mut BC, frame: B);
}

impl<CC, BC, Meta> RecvFrameContext<Meta, BC> for CC
where
    Meta: ReceivableFrameMeta<CC, BC>,
{
    fn receive_frame<B: BufferMut + Debug>(
        &mut self,
        bindings_ctx: &mut BC,
        metadata: Meta,
        frame: B,
    ) {
        metadata.receive_meta(self, bindings_ctx, frame)
    }
}

/// A context for sending frames.
pub trait SendFrameContext<BC, Meta> {
    /// Send a frame.
    ///
    /// `send_frame` sends a frame with the given metadata. The frame itself is
    /// passed as a [`Serializer`] which `send_frame` is responsible for
    /// serializing. If serialization fails for any reason, the original,
    /// unmodified `Serializer` is returned.
    ///
    /// [`Serializer`]: packet::Serializer
    fn send_frame<S>(&mut self, bindings_ctx: &mut BC, metadata: Meta, frame: S) -> Result<(), S>
    where
        S: Serializer,
        S::Buffer: BufferMut;
}

/// A trait providing the send implementation for some frame identified by a
/// metadata type.
///
/// This trait sidesteps orphan rules by allowing [`SendFrameContext`] to be
/// implemented by the multiple core crates, given it can always be implemented
/// for a local metadata type. `SendableFrameMeta` should always be used for
/// trait implementations, while [`SendFrameContext`] is used for trait bounds.
pub trait SendableFrameMeta<CC, BC> {
    /// Sends this frame metadata to the provided contexts.
    fn send_meta<S>(self, core_ctx: &mut CC, bindings_ctx: &mut BC, frame: S) -> Result<(), S>
    where
        S: Serializer,
        S::Buffer: BufferMut;
}

impl<CC, BC, Meta> SendFrameContext<BC, Meta> for CC
where
    Meta: SendableFrameMeta<CC, BC>,
{
    fn send_frame<S>(&mut self, bindings_ctx: &mut BC, metadata: Meta, frame: S) -> Result<(), S>
    where
        S: Serializer,
        S::Buffer: BufferMut,
    {
        metadata.send_meta(self, bindings_ctx, frame)
    }
}

/// The type of address used as the destination address in a device-layer frame.
///
/// `FrameDestination` is used to implement RFC 1122 section 3.2.2 and RFC 4443
/// section 2.4.e, which govern when to avoid sending an ICMP error message for
/// ICMP and ICMPv6 respectively.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FrameDestination {
    /// A unicast address - one which is neither multicast nor broadcast.
    Individual {
        /// Whether the frame's destination address belongs to the receiver.
        local: bool,
    },
    /// A multicast address; if the addressing scheme supports overlap between
    /// multicast and broadcast, then broadcast addresses should use the
    /// `Broadcast` variant.
    Multicast,
    /// A broadcast address; if the addressing scheme supports overlap between
    /// multicast and broadcast, then broadcast addresses should use the
    /// `Broadcast` variant.
    Broadcast,
}

impl FrameDestination {
    /// Is this `FrameDestination::Broadcast`?
    pub fn is_broadcast(self) -> bool {
        self == FrameDestination::Broadcast
    }

    /// Creates a `FrameDestination` from a `mac` and `local_mac` destination.
    pub fn from_dest(destination: Mac, local_mac: Mac) -> Self {
        BroadcastAddr::new(destination)
            .map(Into::into)
            .or_else(|| MulticastAddr::new(destination).map(Into::into))
            .unwrap_or_else(|| FrameDestination::Individual { local: destination == local_mac })
    }
}

impl From<BroadcastAddr<Mac>> for FrameDestination {
    fn from(_value: BroadcastAddr<Mac>) -> Self {
        Self::Broadcast
    }
}

impl From<MulticastAddr<Mac>> for FrameDestination {
    fn from(_value: MulticastAddr<Mac>) -> Self {
        Self::Multicast
    }
}

/// The metadata required for a packet to get into the IP layer.
pub struct RecvIpFrameMeta<D, I: Ip> {
    /// The device on which the IP frame was received.
    pub device: D,
    /// The link-layer destination address from the link-layer frame, if any.
    /// `None` if the IP frame originated above the link-layer (e.g. pure IP
    /// devices).
    // NB: In the future, this field may also be `None` to represent link-layer
    // protocols without destination addresses (i.e. PPP), but at the moment no
    // such protocols are supported.
    pub frame_dst: Option<FrameDestination>,
    /// A marker for the Ip version in this frame.
    pub marker: IpVersionMarker<I>,
}

impl<D, I: Ip> RecvIpFrameMeta<D, I> {
    /// Creates a new `RecvIpFrameMeta` originating from `device` and `frame_dst`
    /// option.
    pub fn new(device: D, frame_dst: Option<FrameDestination>) -> RecvIpFrameMeta<D, I> {
        RecvIpFrameMeta { device, frame_dst, marker: IpVersionMarker::new() }
    }
}

#[cfg(any(test, feature = "testutils"))]
pub(crate) mod testutil {
    use super::*;
    use alloc::{boxed::Box, vec::Vec};

    /// A fake [`FrameContext`].
    pub struct FakeFrameCtx<Meta> {
        frames: Vec<(Meta, Vec<u8>)>,
        should_error_for_frame: Option<Box<dyn FnMut(&Meta) -> bool + Send>>,
    }

    impl<Meta> FakeFrameCtx<Meta> {
        /// Closure which can decide to cause an error to be thrown when
        /// handling a frame, based on the metadata.
        pub fn set_should_error_for_frame<F: Fn(&Meta) -> bool + Send + 'static>(&mut self, f: F) {
            self.should_error_for_frame = Some(Box::new(f));
        }
    }

    impl<Meta> Default for FakeFrameCtx<Meta> {
        fn default() -> FakeFrameCtx<Meta> {
            FakeFrameCtx { frames: Vec::new(), should_error_for_frame: None }
        }
    }

    impl<Meta> FakeFrameCtx<Meta> {
        /// Take all frames sent so far.
        pub fn take_frames(&mut self) -> Vec<(Meta, Vec<u8>)> {
            core::mem::take(&mut self.frames)
        }

        /// Get the frames sent so far.
        pub fn frames(&self) -> &[(Meta, Vec<u8>)] {
            self.frames.as_slice()
        }

        /// Pushes a frame to the context.
        pub fn push(&mut self, meta: Meta, frame: Vec<u8>) {
            self.frames.push((meta, frame))
        }
    }

    impl<Meta, BC> SendableFrameMeta<FakeFrameCtx<Meta>, BC> for Meta {
        fn send_meta<S>(
            self,
            core_ctx: &mut FakeFrameCtx<Meta>,
            _bindings_ctx: &mut BC,
            frame: S,
        ) -> Result<(), S>
        where
            S: Serializer,
            S::Buffer: BufferMut,
        {
            if let Some(should_error_for_frame) = core_ctx.should_error_for_frame.as_mut() {
                if should_error_for_frame(&self) {
                    return Err(frame);
                }
            }

            let buffer = frame.serialize_vec_outer().map_err(|(_err, s)| s)?;
            core_ctx.push(self, buffer.as_ref().to_vec());
            Ok(())
        }
    }

    /// A trait for abstracting contexts that may contain a [`FakeFrameCtx`].
    pub trait WithFakeFrameContext<SendMeta> {
        /// Calls the callback with a mutable reference to the [`FakeFrameCtx`].
        fn with_fake_frame_ctx_mut<O, F: FnOnce(&mut FakeFrameCtx<SendMeta>) -> O>(
            &mut self,
            f: F,
        ) -> O;
    }

    impl<SendMeta> WithFakeFrameContext<SendMeta> for FakeFrameCtx<SendMeta> {
        fn with_fake_frame_ctx_mut<O, F: FnOnce(&mut FakeFrameCtx<SendMeta>) -> O>(
            &mut self,
            f: F,
        ) -> O {
            f(self)
        }
    }
}
