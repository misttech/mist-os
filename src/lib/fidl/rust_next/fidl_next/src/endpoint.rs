// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::marker::PhantomData;

use crate::{
    munge, Decode, DecodeError, Encodable, EncodableOption, Encode, EncodeError, EncodeOption,
    Slot, TakeFrom,
};

/// A marker type for client endpoints (temporary)
#[derive(Debug)]
pub struct ClientEndpoint;

/// A marker type for server ends of protocols (temporary)
#[derive(Debug)]
pub struct ServerEndpoint;

/// A resource associated with an endpoint.
#[derive(Debug)]
#[repr(transparent)]
pub struct EndpointResource<T, P> {
    resource: T,
    _endpoint: PhantomData<P>,
}

impl<T, P> EndpointResource<T, P> {
    /// Returns a new endpoint over the given resource.
    pub fn new(resource: T) -> Self {
        Self { resource, _endpoint: PhantomData }
    }

    /// Returns the underlying resource.
    pub fn into_inner(self) -> T {
        self.resource
    }
}

unsafe impl<D: ?Sized, T, P> Decode<D> for EndpointResource<T, P>
where
    T: Decode<D>,
{
    fn decode(slot: Slot<'_, Self>, decoder: &mut D) -> Result<(), DecodeError> {
        munge!(let Self { resource, _endpoint: _ } = slot);
        T::decode(resource, decoder)
    }
}

impl<T, P> Encodable for EndpointResource<T, P>
where
    T: Encodable,
{
    type Encoded<'buf> = EndpointResource<T::Encoded<'buf>, P>;
}

impl<T, P> EncodableOption for EndpointResource<T, P>
where
    T: EncodableOption,
{
    type EncodedOption<'buf> = EndpointResource<T::EncodedOption<'buf>, P>;
}

impl<E, T, P> Encode<E> for EndpointResource<T, P>
where
    E: ?Sized,
    T: Encode<E>,
{
    fn encode(
        &mut self,
        encoder: &mut E,
        slot: Slot<'_, Self::Encoded<'_>>,
    ) -> Result<(), EncodeError> {
        munge!(let Self::Encoded { resource, _endpoint: _ } = slot);
        self.resource.encode(encoder, resource)
    }
}

impl<E, T, P> EncodeOption<E> for EndpointResource<T, P>
where
    E: ?Sized,
    T: EncodeOption<E>,
{
    fn encode_option(
        this: Option<&mut Self>,
        encoder: &mut E,
        slot: Slot<'_, Self::EncodedOption<'_>>,
    ) -> Result<(), EncodeError> {
        munge!(let Self::EncodedOption { resource, _endpoint: _ } = slot);
        T::encode_option(this.map(|this| &mut this.resource), encoder, resource)
    }
}

impl<T, P, U> TakeFrom<EndpointResource<U, P>> for EndpointResource<T, P>
where
    T: TakeFrom<U>,
{
    fn take_from(from: &mut EndpointResource<U, P>) -> Self {
        Self { resource: T::take_from(&mut from.resource), _endpoint: PhantomData }
    }
}
