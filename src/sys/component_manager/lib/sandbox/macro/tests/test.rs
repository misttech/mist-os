// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_component_sandbox as fsandbox;
use sandbox::{AnyCapability, Capability, ConversionError, ErasedCapability};
use std::any::{Any, TypeId};
use std::borrow::BorrowMut;
use std::fmt::Debug;

/// A capability that holds a string.
#[derive(Capability, Clone, Debug)]
#[capability(as_trait(ReadString))]
struct StringCapability(pub String);

impl Capability for StringCapability {
    fn try_into_capability(self, type_id: TypeId) -> Result<Box<dyn Any>, ConversionError> {
        if type_id == TypeId::of::<Self>() {
            return Ok(Box::new(self) as Box<dyn Any>);
        } else if type_id == TypeId::of::<IntCapability>() {
            // StringCapability converted to IntCapability holds the string's length.
            let int_cap = IntCapability(self.0.len() as i64);
            return Ok(Box::new(int_cap));
        }
        Err(ConversionError::NotSupported)
    }
}

trait ReadString {
    fn read(&self) -> String;
}

impl ReadString for StringCapability {
    fn read(&self) -> String {
        self.0.clone()
    }
}

impl From<StringCapability> for fsandbox::Capability {
    fn from(_capability: StringCapability) -> Self {
        unimplemented!()
    }
}

/// A capability that holds an integer.
#[derive(Capability, Clone, Debug)]
struct IntCapability(pub i64);

impl Capability for IntCapability {}

impl From<IntCapability> for fsandbox::Capability {
    fn from(_capability: IntCapability) -> Self {
        unimplemented!()
    }
}

/// Tests that the derived TryFrom<AnyCapability> impl can downcast the capability into Self.
#[test]
fn test_try_from_any_into_self() {
    let cap = StringCapability("hello".to_string());
    let any: AnyCapability = Box::new(cap);

    // Downcast back into StringCapability.
    let cap: StringCapability = any.try_into().expect("failed to downcast");

    assert_eq!(cap.0, "hello".to_string());
}

/// Tests that the derived TryFrom<AnyCapability> impl can convert the capability into
/// a non-Self type using [Convert.try_into_capability].
#[test]
fn test_try_from_any_convert() {
    let cap = StringCapability("hello".to_string());
    let any: AnyCapability = Box::new(cap);

    // Convert from type-erased StringCapability into IntCapability.
    let int_cap: IntCapability = any.try_into().expect("failed to convert");

    assert_eq!(int_cap.0, "hello".len() as i64);
}

/// Tests that the default try_into_capability impl succeeds in converting to the `Self` type.
#[test]
fn test_convert_to_self_only() {
    let cap = StringCapability("hello".to_string());
    let converted: Box<dyn Any> =
        cap.try_into_capability(TypeId::of::<StringCapability>()).unwrap();
    let cap: StringCapability = *converted.downcast::<StringCapability>().unwrap();
    assert_eq!(cap.0, "hello".to_string());
}

/// Tests that the default try_into_capability impl returns an error when trying to convert to a
/// non-`Self` type.
#[test]
fn test_convert_to_self_only_wrong_type() {
    let cap = IntCapability(123);
    let convert_result = cap.try_into_capability(TypeId::of::<StringCapability>());
    // IntCapability can only be converted to itself, not to StringCapability.
    assert!(convert_result.is_err());
}

/// Tests the `TryFrom<&AnyCapability>` to reference downcast conversion.
#[test]
fn try_from_any_ref() {
    let cap = StringCapability("hello".to_string());
    let any: AnyCapability = Box::new(cap);

    let from: &AnyCapability = &any;
    let to: &StringCapability = from.try_into().unwrap();

    assert_eq!(to.0, "hello".to_string());
}

/// Tests the `TryFrom<&mut AnyCapability>` to mut reference downcast conversion.
#[test]
fn try_from_any_mut_ref() {
    let cap = StringCapability("hello".to_string());
    let mut any: AnyCapability = Box::new(cap);

    let from: &mut AnyCapability = &mut any;
    let to: &mut StringCapability = from.try_into().unwrap();

    assert_eq!(to.0, "hello".to_string());
}

/// Tests the `TryFrom<&dyn ErasedCapability>` to reference downcast conversion.
#[test]
fn try_from_dyn_erased_ref() {
    let cap = StringCapability("hello".to_string());
    let any: AnyCapability = Box::new(cap);

    let from: &dyn ErasedCapability = any.as_ref();
    let to: &StringCapability = from.try_into().unwrap();

    assert_eq!(to.0, "hello".to_string());
}

/// Tests the `TryFrom<&mut dyn ErasedCapability>` to mut reference downcast conversion.
#[test]
fn try_from_dyn_erased_mut_ref() {
    let cap = StringCapability("hello".to_string());
    let mut any: AnyCapability = Box::new(cap);

    let from: &mut dyn ErasedCapability = any.borrow_mut();
    let to: &mut StringCapability = from.try_into().unwrap();

    assert_eq!(to.0, "hello".to_string());
}
