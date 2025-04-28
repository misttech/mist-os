// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::marker::PhantomData;
use core::mem::MaybeUninit;
use core::{concat, stringify};

use fidl_next_codec::{
    munge, Decode, DecodeError, Encodable, EncodableOption, Encode, EncodeError, EncodeOption,
    EncodeRef, Slot, TakeFrom, ZeroPadding,
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

        // SAFETY: `$name` is `#[repr(transparent)]` over the transport `T`, and `zero_padding`
        // calls `T::zero_padding` on `transport`. `_protocol` is a ZST which does not have any
        // padding bytes to zero-initialize.
        unsafe impl<T: ZeroPadding, P> ZeroPadding for $name<T, P> {
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
        {
            fn decode(slot: Slot<'_, Self>, decoder: &mut D) -> Result<(), DecodeError> {
                munge!(let Self { transport, _protocol: _ } = slot);
                T::decode(transport, decoder)
            }
        }

        impl<T, P> Encodable for $name<T, P>
        where
            T: Encodable,
        {
            type Encoded = $name<T::Encoded, P>;
        }

        impl<T, P> EncodableOption for $name<T, P>
        where
            T: EncodableOption,
        {
            type EncodedOption = $name<T::EncodedOption, P>;
        }

        // SAFETY: `$name` is `#[repr(transparent)]` over the transport `T`, and `encode` calls
        // `T::encode` on `transport`. `_protocol` is a ZST which does not have any data to encode.
        unsafe impl<E, T, P> Encode<E> for $name<T, P>
        where
            E: ?Sized,
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
        unsafe impl<E, T, P> EncodeRef<E> for $name<T, P>
        where
            E: ?Sized,
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
        unsafe impl<E, T, P> EncodeOption<E> for $name<T, P>
        where
            E: ?Sized,
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

        impl<T, P, U> TakeFrom<$name<U, P>> for $name<T, P>
        where
            T: TakeFrom<U>,
        {
            fn take_from(from: &$name<U, P>) -> Self {
                Self { transport: T::take_from(&from.transport), _protocol: PhantomData }
            }
        }

        impl<T, P, U> TakeFrom<$name<U, P>> for Option<$name<T, P>>
        where
            Option<T>: TakeFrom<U>,
        {
            fn take_from(from: &$name<U, P>) -> Self {
                Option::<T>::take_from(&from.transport)
                    .map(|transport| $name { transport, _protocol: PhantomData })
            }
        }

        #[cfg(feature = "compat")]
        impl<T, P1, P2> TakeFrom<$name<T, P1>> for ::fidl::endpoints::$name<P2>
        where
            ::fidl::Channel: TakeFrom<T>,
            P2: TakeFrom<P1>,
        {
            fn take_from(from: &$name<T, P1>) -> Self {
                Self::new(::fidl::Channel::take_from(&from.transport))
            }
        }

        #[cfg(feature = "compat")]
        impl<T, P1, P2> TakeFrom<$name<T, P1>> for Option<::fidl::endpoints::$name<P2>>
        where
            Option<::fidl::Channel>: TakeFrom<T>,
            P2: TakeFrom<P1>,
        {
            fn take_from(from: &$name<T, P1>) -> Self {
                Option::<::fidl::Channel>::take_from(&from.transport)
                    .map(::fidl::endpoints::$name::new)
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
