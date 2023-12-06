// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use fidl_fuchsia_component_sandbox as fsandbox;

use crate::{AnyCapability, AnyCast, Capability, ConversionError};

/// A capability that contains an Option of a capability.
#[derive(Capability, Clone, Debug)]
pub struct Optional(pub Option<AnyCapability>);

impl Optional {
    /// Returns an optional that holds None.
    pub fn void() -> Self {
        Optional(None)
    }

    /// Returns an Optional that contains the value.
    ///
    /// If the value is already an Optional of the same type, returns it as-is.
    pub fn from_any(value: AnyCapability) -> Self {
        if (*value).as_any().is::<Self>() {
            *value.into_any().downcast::<Self>().unwrap()
        } else {
            Self(Some(value))
        }
    }
}

impl Capability for Optional {
    fn try_into_capability(
        self,
        type_id: std::any::TypeId,
    ) -> Result<Box<dyn std::any::Any>, ConversionError> {
        if type_id == std::any::TypeId::of::<Self>() {
            return Ok(Box::new(self).into_any());
        }
        match self.0 {
            None => Err(ConversionError::NotSupported),
            Some(cap) => cap.try_into_capability(type_id),
        }
    }
}

impl From<Optional> for fsandbox::OptionalCapability {
    fn from(optional: Optional) -> Self {
        Self { value: optional.0.map(|value| Box::new(value.into_fidl())) }
    }
}

impl From<Optional> for fsandbox::Capability {
    fn from(optional: Optional) -> Self {
        Self::Optional(optional.into())
    }
}

#[cfg(test)]
mod test {
    use super::Optional;
    use crate::{AnyCapability, Unit};
    use assert_matches::assert_matches;
    use fidl_fuchsia_component_sandbox as fsandbox;

    #[test]
    fn test_void() {
        let void = Optional::void();
        assert!(void.0.is_none());
    }

    #[test]
    fn test_from_any() {
        let cap: AnyCapability = Box::new(Unit::default());
        let optional = Optional::from_any(cap);

        assert!(optional.0.is_some());
        assert!(optional.0.unwrap().as_any().is::<Unit>());
    }

    #[test]
    fn test_from_any_from_optional() {
        let cap: AnyCapability = Box::new(Unit::default());
        let optional_any: AnyCapability = Box::new(Optional(Some(cap)));

        // Convert from an AnyCapability that is already an Optional to Optional.
        let optional = Optional::from_any(optional_any);

        assert!(optional.0.is_some());
        assert!(optional.0.unwrap().as_any().is::<Unit>());
    }

    // Tests that `try_into_capability` can convert the inner Some value to its concrete type.
    #[test]
    fn test_try_into_inner() {
        let cap: AnyCapability = Box::new(Unit::default());
        let optional_any: AnyCapability = Box::new(Optional(Some(cap)));

        let unit: Unit = optional_any.try_into().expect("failed to convert to Unit");

        assert_eq!(unit, Unit::default());
    }

    #[test]
    fn test_void_into_fidl() {
        let unit = Optional::void();
        let any: AnyCapability = Box::new(unit);
        let fidl_capability: fsandbox::Capability = any.into();
        assert_eq!(
            fidl_capability,
            fsandbox::Capability::Optional(fsandbox::OptionalCapability { value: None })
        );
    }

    #[test]
    fn test_some_into_fidl() {
        let cap: AnyCapability = Box::new(Unit::default());
        let optional_any: AnyCapability = Box::new(Optional(Some(cap)));
        let fidl_capability: fsandbox::Capability = optional_any.into();
        assert_matches!(
            fidl_capability,
            fsandbox::Capability::Optional(fsandbox::OptionalCapability {
                value: Some(value)
            })
            if *value == fsandbox::Capability::Unit(fsandbox::UnitCapability {})
        );
    }
}
