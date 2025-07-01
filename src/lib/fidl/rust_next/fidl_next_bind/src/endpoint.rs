// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::marker::PhantomData;
use core::mem::MaybeUninit;
use core::{concat, stringify};

use fidl_next_codec::{
    munge, Decode, DecodeError, Encodable, EncodableOption, Encode, EncodeError, EncodeOption,
    EncodeOptionRef, EncodeRef, FromWire, FromWireOption, FromWireOptionRef, FromWireRef, Slot,
    Wire,
};

macro_rules! endpoint {
    (
        #[doc = $doc:literal]
        $name:ident
    ) => {
        #[doc = $doc]
        #[derive(Debug)]
        #[repr(transparent)]
        pub struct $name<
            P,
            #[cfg(feature = "fuchsia")]
            T = zx::Channel,
            #[cfg(not(feature = "fuchsia"))]
            T,
        > {
            transport: T,
            _protocol: PhantomData<P>,
        }

        // SAFETY:
        // - `$name::Decoded<'de>` wraps a `T::Decoded<'de>`. Because `T: Wire`, `T::Decoded<'de>`
        //   does not yield any references to decoded data that outlive `'de`. Therefore,
        //   `$name::Decoded<'de>` also does not yield any references to decoded data that outlive
        //   `'de`.
        // - `$name` is `#[repr(transparent)]` over the transport `T`, and `zero_padding` calls
        //   `T::zero_padding` on `transport`. `_protocol` is a ZST which does not have any padding
        //   bytes to zero-initialize.
        unsafe impl<P: 'static, T: Wire> Wire for $name<P, T> {
            type Decoded<'de> = $name<P, T::Decoded<'de>>;

            #[inline]
            fn zero_padding(out: &mut MaybeUninit<Self>) {
                munge!(let Self { transport, _protocol: _ } = out);
                T::zero_padding(transport);
            }
        }

        impl<P, T> $name<P, T> {
            #[doc = concat!(
                "Converts from `&",
                stringify!($name),
                "<P, T>` to `",
                stringify!($name),
                "<P, &T>`.",
            )]
            pub fn as_ref(&self) -> $name<P, &T> {
                $name { transport: &self.transport, _protocol: PhantomData }
            }

            /// Returns a new endpoint over the given transport.
            pub fn from_untyped(transport: T) -> Self {
                Self { transport, _protocol: PhantomData }
            }

            /// Returns the underlying transport.
            pub fn into_untyped(self) -> T {
                self.transport
            }
        }

        // SAFETY: `$name` is `#[repr(transparent)]` over the transport `T`, and `decode` calls
        // `T::decode` on `transport`. `_protocol` is a ZST which does not have any data to decode.
        unsafe impl<D, P, T> Decode<D> for $name<P, T>
        where
            D: ?Sized,
            P: 'static,
            T: Decode<D>,
        {
            fn decode(slot: Slot<'_, Self>, decoder: &mut D) -> Result<(), DecodeError> {
                munge!(let Self { transport, _protocol: _ } = slot);
                T::decode(transport, decoder)
            }
        }

        impl<P, T> Encodable for $name<P, T>
        where
            T: Encodable,
            P: 'static,
        {
            type Encoded = $name<P, T::Encoded>;
        }

        impl<P, T> EncodableOption for $name<P, T>
        where
            T: EncodableOption,
            P: 'static,
        {
            type EncodedOption = $name<P, T::EncodedOption>;
        }

        // SAFETY: `$name` is `#[repr(transparent)]` over the transport `T`, and `encode` calls
        // `T::encode` on `transport`. `_protocol` is a ZST which does not have any data to encode.
        unsafe impl<E, P, T> Encode<E> for $name<P, T>
        where
            E: ?Sized,
            P: 'static,
            T: Encode<E>,
        {
            fn encode(
                self,
                encoder: &mut E,
                out: &mut MaybeUninit<Self::Encoded>,
            ) -> Result<(), EncodeError> {
                munge!(let Self::Encoded { transport, _protocol: _ } = out);
                self.transport.encode(encoder, transport)
            }
        }

        // SAFETY: `$name` is `#[repr(transparent)]` over the transport `T`, and `encode_ref` calls
        // `T::encode_ref` on `transport`. `_protocol` is a ZST which does not have any data to
        // encode.
        unsafe impl<E, P, T> EncodeRef<E> for $name<P, T>
        where
            E: ?Sized,
            P: 'static,
            T: EncodeRef<E>,
        {
            fn encode_ref(
                &self,
                encoder: &mut E,
                out: &mut MaybeUninit<Self::Encoded>,
            ) -> Result<(), EncodeError> {
                self.as_ref().encode(encoder, out)
            }
        }

        // SAFETY: `$name` is `#[repr(transparent)]` over the transport `T`, and `encode_option`
        // calls `T::encode_option` on `transport`. `_protocol` is a ZST which does not have any
        // data to encode.
        unsafe impl<E, P, T> EncodeOption<E> for $name<P, T>
        where
            E: ?Sized,
            P: 'static,
            T: EncodeOption<E>,
        {
            fn encode_option(
                this: Option<Self>,
                encoder: &mut E,
                out: &mut MaybeUninit<Self::EncodedOption>,
            ) -> Result<(), EncodeError> {
                munge!(let Self::EncodedOption { transport, _protocol: _ } = out);
                T::encode_option(this.map(|this| this.transport), encoder, transport)
            }
        }

        // SAFETY: `$name` is `#[repr(transparent)]` over the transport `T`, and `encode_option_ref`
        // calls `T::encode_option_ref` on `transport`. `_protocol` is a ZST which does not have any
        // data to encode.
        unsafe impl<E, P, T> EncodeOptionRef<E> for $name<P, T>
        where
            E: ?Sized,
            P: 'static,
            T: EncodeOptionRef<E>,
        {
            fn encode_option_ref(
                this: Option<&Self>,
                encoder: &mut E,
                out: &mut MaybeUninit<Self::EncodedOption>,
            ) -> Result<(), EncodeError> {
                munge!(let Self::EncodedOption { transport, _protocol: _ } = out);
                T::encode_option_ref(this.map(|this| &this.transport), encoder, transport)
            }
        }

        impl<P, T, U> FromWire<$name<P, U>> for $name<P, T>
        where
            T: FromWire<U>,
        {
            #[inline]
            fn from_wire(wire: $name<P, U>) -> Self {
                $name {
                    transport: T::from_wire(wire.transport),
                    _protocol: PhantomData,
                }
            }
        }

        impl<P, T, U> FromWireRef<$name<P, U>> for $name<P, T>
        where
            T: FromWireRef<U>,
        {
            #[inline]
            fn from_wire_ref(wire: &$name<P, U>) -> Self {
                $name {
                    transport: T::from_wire_ref(&wire.transport),
                    _protocol: PhantomData,
                }
            }
        }

        impl<P, T, U> FromWireOption<$name<P, U>> for $name<P, T>
        where
            P: 'static,
            T: FromWireOption<U>,
            U: Wire,
        {
            #[inline]
            fn from_wire_option(wire: $name<P, U>) -> Option<Self> {
                T::from_wire_option(wire.transport).map(|transport| $name {
                    transport,
                    _protocol: PhantomData,
                })
            }
        }

        impl<P, T, U> FromWireOptionRef<$name<P, U>> for $name<P, T>
        where
            P: 'static,
            T: FromWireOptionRef<U>,
            U: Wire,
        {
            #[inline]
            fn from_wire_option_ref(wire: &$name<P, U>) -> Option<Self> {
                T::from_wire_option_ref(&wire.transport).map(|transport| $name {
                    transport,
                    _protocol: PhantomData,
                })
            }
        }

        #[cfg(feature = "compat")]
        impl<P1, P2, T> From<$name<P1, T>> for ::fidl::endpoints::$name<P2>
        where
            ::fidl::Channel: From<T>,
            P2: From<P1>,
        {
            fn from(from: $name<P1, T>) -> Self {
                Self::new(from.transport.into())
            }
        }
    };
}

endpoint! {
    /// The client end of a protocol.
    ClientEnd
}

endpoint! {
    /// The server end of a protocol.
    ServerEnd
}
