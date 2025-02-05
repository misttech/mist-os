// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::marker::PhantomData;

use fidl_next_codec::{
    munge, Decode, DecodeError, Encodable, EncodableOption, Encode, EncodeError, EncodeOption,
    Slot, TakeFrom,
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
            type Encoded<'buf> = $name<T::Encoded<'buf>, P>;
        }

        impl<T, P> EncodableOption for $name<T, P>
        where
            T: EncodableOption,
        {
            type EncodedOption<'buf> = $name<T::EncodedOption<'buf>, P>;
        }

        impl<E, T, P> Encode<E> for $name<T, P>
        where
            E: ?Sized,
            T: Encode<E>,
        {
            fn encode(
                &mut self,
                encoder: &mut E,
                slot: Slot<'_, Self::Encoded<'_>>,
            ) -> Result<(), EncodeError> {
                munge!(let Self::Encoded { transport, _protocol: _ } = slot);
                self.transport.encode(encoder, transport)
            }
        }

        impl<E, T, P> EncodeOption<E> for $name<T, P>
        where
            E: ?Sized,
            T: EncodeOption<E>,
        {
            fn encode_option(
                this: Option<&mut Self>,
                encoder: &mut E,
                slot: Slot<'_, Self::EncodedOption<'_>>,
            ) -> Result<(), EncodeError> {
                munge!(let Self::EncodedOption { transport, _protocol: _ } = slot);
                T::encode_option(this.map(|this| &mut this.transport), encoder, transport)
            }
        }

        impl<T, P, U> TakeFrom<$name<U, P>> for $name<T, P>
        where
            T: TakeFrom<U>,
        {
            fn take_from(from: &mut $name<U, P>) -> Self {
                Self { transport: T::take_from(&mut from.transport), _protocol: PhantomData }
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
