// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon as zx;
use futures::future::BoxFuture;
use sandbox::{AnyCapability, AnyCast, Capability};

/// A capability that contains an Option of a capability.
#[derive(Capability, Debug)]
pub struct Optional(pub Option<AnyCapability>);

impl Optional {
    /// Returns an optional that holds None.
    pub fn void() -> Self {
        Optional(None)
    }

    /// Returns an Optional that contains the value.
    ///
    /// If the value is already an Optional of the same type, returns it as-is.
    // TODO(b/298112397): Fix the blanket `impl<T: HandleBased> From<T> for AnyCapability` impl so
    // this method can be a From and used like `let optional: Optional = any.into()`
    pub fn from_any(value: AnyCapability) -> Self {
        if (*value).as_any().is::<Self>() {
            *value.into_any().downcast::<Self>().unwrap()
        } else {
            Self(Some(value))
        }
    }
}

impl Capability for Optional {
    fn to_zx_handle(self) -> (zx::Handle, Option<BoxFuture<'static, ()>>) {
        match self.0 {
            None => (zx::Handle::invalid(), None),
            Some(cap) => cap.to_zx_handle(),
        }
    }

    fn try_clone(&self) -> Result<Self, ()> {
        match &self.0 {
            None => Ok(Optional(None)),
            Some(cap) => Ok(Optional(Some(cap.try_clone()?))),
        }
    }

    fn try_into_capability(self, type_id: std::any::TypeId) -> Result<Box<dyn std::any::Any>, ()> {
        if type_id == std::any::TypeId::of::<Self>() {
            return Ok(Box::new(self).into_any());
        }
        match self.0 {
            None => Err(()),
            Some(cap) => cap.try_into_capability(type_id),
        }
    }
}

#[cfg(test)]
mod test {
    use super::Optional;
    use sandbox::{AnyCapability, Data};

    #[test]
    fn test_void() {
        let void = Optional::void();
        assert!(void.0.is_none());
    }

    #[test]
    fn test_from_any() {
        let cap: AnyCapability = Box::new(Data::new("hello".to_string()));
        let optional = Optional::from_any(cap);

        assert!(optional.0.is_some());
        assert!(optional.0.unwrap().as_any().is::<Data<String>>());
    }

    #[test]
    fn test_from_any_from_optional() {
        let cap: AnyCapability = Box::new(Data::new("hello".to_string()));
        let optional_any: AnyCapability = Box::new(Optional(Some(cap)));

        // Convert from an AnyCapability that is already an Optional to Optional.
        let optional = Optional::from_any(optional_any);

        assert!(optional.0.is_some());
        assert!(optional.0.unwrap().as_any().is::<Data<String>>());
    }

    // Tests that `try_into_capability` can convert the inner Some value to its concrete type.
    #[test]
    fn test_try_into_inner() {
        let cap: AnyCapability = Box::new(Data::new("hello".to_string()));
        let optional_any: AnyCapability = Box::new(Optional(Some(cap)));

        let data: Data<String> =
            optional_any.try_into().expect("failed to convert to Data<String>");

        assert_eq!(data.value, "hello");
    }
}
