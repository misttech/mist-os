// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::marker::PhantomData;
use core::mem::MaybeUninit;
use core::{concat, stringify};

use fidl_next_codec::{
    munge, Decode, DecodeError, Encodable, EncodableOption, Encode, EncodeError, EncodeOption,
    EncodeRef, FromWire, FromWireOption, Slot, Wire,
};

macro_rules! endpoint {
    (
        #[doc = $doc:literal]
        $name:ident
    ) => {
        #[doc = $doc]
        #[derive(Debug)]
        #[repr(transparent)]
        pub struct $name<T, P> {
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
        unsafe impl<T: Wire, P: 'static> Wire for $name<T, P> {
            type Decoded<'de> = $name<T::Decoded<'de>, P>;

            #[inline]
            fn zero_padding(out: &mut MaybeUninit<Self>) {
                munge!(let Self { transport, _protocol: _ } = out);
                T::zero_padding(transport);
            }
        }

        impl<T, P> $name<T, P> {
            #[doc = concat!(
                "Converts from `&",
                stringify!($name),
                "<T, P>` to `",
                stringify!($name),
                "<&T, P>`.",
            )]
            pub fn as_ref(&self) -> $name<&T, P> {
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
        unsafe impl<D, T, P> Decode<D> for $name<T, P>
        where
            D: ?Sized,
            T: Decode<D>,
            P: 'static,
        {
            fn decode(slot: Slot<'_, Self>, decoder: &mut D) -> Result<(), DecodeError> {
                munge!(let Self { transport, _protocol: _ } = slot);
                T::decode(transport, decoder)
            }
        }

        impl<T, P> Encodable for $name<T, P>
        where
            T: Encodable,
            P: 'static,
        {
            type Encoded = $name<T::Encoded, P>;
        }

        impl<T, P> EncodableOption for $name<T, P>
        where
            T: EncodableOption,
            P: 'static,
        {
            type EncodedOption = $name<T::EncodedOption, P>;
        }

        // SAFETY: `$name` is `#[repr(transparent)]` over the transport `T`, and `encode` calls
        // `T::encode` on `transport`. `_protocol` is a ZST which does not have any data to encode.
        unsafe impl<E, T, P> Encode<E> for $name<T, P>
        where
            E: ?Sized,
            T: Encode<E>,
            P: 'static,
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
        unsafe impl<E, T, P> EncodeRef<E> for $name<T, P>
        where
            E: ?Sized,
            T: EncodeRef<E>,
            P: 'static,
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
        unsafe impl<E, T, P> EncodeOption<E> for $name<T, P>
        where
            E: ?Sized,
            T: EncodeOption<E>,
            P: 'static,
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

        impl<T, P, U> FromWire<$name<U, P>> for $name<T, P>
        where
            T: FromWire<U>,
        {
            fn from_wire(wire: $name<U, P>) -> Self {
                $name {
                    transport: T::from_wire(wire.transport),
                    _protocol: PhantomData,
                }
            }
        }

        impl<T, P, U> FromWireOption<$name<U, P>> for $name<T, P>
        where
            T: FromWireOption<U>,
            U: Wire,
            P: 'static,
        {
            fn from_wire_option(wire: $name<U, P>) -> Option<Self> {
                T::from_wire_option(wire.transport).map(|transport| $name {
                    transport,
                    _protocol: PhantomData,
                })
            }
        }

        #[cfg(feature = "compat")]
        impl<T, P1, P2> From<$name<T, P1>> for ::fidl::endpoints::$name<P2>
        where
            ::fidl::Channel: From<T>,
            P2: From<P1>,
        {
            fn from(from: $name<T, P1>) -> Self {
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
