// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::component_instance::{
    ComponentInstanceInterface, ExtendedInstanceInterface, WeakComponentInstanceInterface,
    WeakExtendedInstanceInterface,
};
use crate::error::ComponentInstanceError;
use moniker::ExtendedMoniker;
use sandbox::WeakInstanceToken;
use std::sync::Arc;

/// A trait to add functions WeakComponentInstancethat know about the component
/// manager types.
pub trait WeakInstanceTokenExt<C: ComponentInstanceInterface + 'static> {
    /// Upgrade this token to the underlying instance.
    fn to_instance(self) -> WeakExtendedInstanceInterface<C>;

    /// Get a reference to the underlying instance.
    fn as_ref(&self) -> &WeakExtendedInstanceInterface<C>;

    /// Get a strong reference to the underlying instance.
    fn upgrade(&self) -> Result<ExtendedInstanceInterface<C>, ComponentInstanceError>;

    /// Get the moniker for this component.
    fn moniker(&self) -> ExtendedMoniker;
}

impl<C: ComponentInstanceInterface + 'static> WeakInstanceTokenExt<C> for WeakInstanceToken {
    fn to_instance(self) -> WeakExtendedInstanceInterface<C> {
        self.as_ref().clone()
    }

    fn as_ref(&self) -> &WeakExtendedInstanceInterface<C> {
        match self.inner.as_any().downcast_ref::<WeakExtendedInstanceInterface<C>>() {
            Some(instance) => &instance,
            None => panic!(),
        }
    }

    fn upgrade(&self) -> Result<ExtendedInstanceInterface<C>, ComponentInstanceError> {
        self.as_ref().upgrade()
    }

    fn moniker(&self) -> ExtendedMoniker {
        WeakInstanceTokenExt::<C>::as_ref(self).extended_moniker()
    }
}

/// Returns a token representing an invalid component. For testing.
pub fn test_invalid_instance_token<C: ComponentInstanceInterface + 'static>() -> WeakInstanceToken {
    WeakComponentInstanceInterface::<C>::invalid().into()
}

impl<C: ComponentInstanceInterface + 'static> From<WeakExtendedInstanceInterface<C>>
    for WeakInstanceToken
{
    fn from(instance: WeakExtendedInstanceInterface<C>) -> Self {
        Self { inner: Arc::new(instance) }
    }
}

impl<C: ComponentInstanceInterface + 'static> From<WeakComponentInstanceInterface<C>>
    for WeakInstanceToken
{
    fn from(instance: WeakComponentInstanceInterface<C>) -> Self {
        Self::from(WeakExtendedInstanceInterface::<C>::Component(instance))
    }
}
