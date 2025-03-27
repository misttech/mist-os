// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::marker::PhantomData;
use core::mem::MaybeUninit;

use fidl_next_codec::{
    munge, Decode, DecodeError, Encodable, EncodableOption, Encode, EncodeError, EncodeOption,
    Slot, TakeFrom, ZeroPadding,
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

        unsafe impl<T: ZeroPadding, P> ZeroPadding for $name<T, P> {
            #[inline]
            fn zero_padding(out: &mut MaybeUninit<Self>) {
                munge!(let Self { transport, _protocol: _ } = out);
                T::zero_padding(transport);
            }
        }

        impl<T, P> $name<T, P> {
            /// Returns a new endpoint over the given transport.
            pub fn from_untyped(transport: T) -> Self {
                Self { transport, _protocol: PhantomData }
            }

            /// Returns the underlying transport.
            pub fn into_untyped(self) -> T {
                self.transport
            }
        }

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

        unsafe impl<E, T, P> Encode<E> for $name<T, P>
        where
            E: ?Sized,
            T: Encode<E>,
        {
            fn encode(
                &mut self,
                encoder: &mut E,
                out: &mut MaybeUninit<Self::Encoded>,
            ) -> Result<(), EncodeError> {
                munge!(let Self::Encoded { transport, _protocol: _ } = out);
                self.transport.encode(encoder, transport)
            }
        }

        unsafe impl<E, T, P> EncodeOption<E> for $name<T, P>
        where
            E: ?Sized,
            T: EncodeOption<E>,
        {
            fn encode_option(
                this: Option<&mut Self>,
                encoder: &mut E,
                out: &mut MaybeUninit<Self::EncodedOption>,
            ) -> Result<(), EncodeError> {
                munge!(let Self::EncodedOption { transport, _protocol: _ } = out);
                T::encode_option(this.map(|this| &mut this.transport), encoder, transport)
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
