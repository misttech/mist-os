// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon::{self as zx, AsHandleRef};
use futures::future::BoxFuture;
use sandbox::{AnyCapability, Capability, CloneError, ConversionError, ErasedCapability, Handle};
use std::any::{Any, TypeId};
use std::borrow::BorrowMut;
use std::fmt::Debug;

/// A test-only capability that derives [Capability].
#[derive(Capability, Debug)]
struct TestHandle(zx::Handle);

impl Capability for TestHandle {
    fn to_zx_handle(self) -> (zx::Handle, Option<BoxFuture<'static, ()>>) {
        (self.0, None)
    }

    fn try_into_capability(self, type_id: TypeId) -> Result<Box<dyn Any>, ConversionError> {
        assert_eq!(type_id, TypeId::of::<Handle>());
        Ok(Box::new(Handle::from(self.0)))
    }
}

/// Tests that the derived TryFrom<AnyCapability> impl can downcast the capability into Self.
#[test]
fn test_try_from_any_into_self() {
    let event = zx::Event::create();
    let expected_koid = event.get_koid().unwrap();

    let cap = TestHandle(event.into());
    let any: AnyCapability = Box::new(cap);

    // Downcast back into TestHandle.
    let cap: TestHandle = any.try_into().expect("failed to downcast");

    assert_eq!(cap.0.get_koid().unwrap(), expected_koid);
}

/// Tests that the derived TryFrom<AnyCapability> impl can convert the capability into
/// a non-Self type using [Convert.try_into_capability].
#[test]
fn test_try_from_any_convert() {
    let event = zx::Event::create();
    let expected_koid = event.get_koid().unwrap();

    let cap = TestHandle(event.into());
    let any: AnyCapability = Box::new(cap);

    // Convert from type-erased TestHandle into Handle.
    let cap: Handle = any.try_into().expect("failed to convert");

    assert_eq!(cap.as_handle_ref().get_koid().unwrap(), expected_koid);
}

#[test]
fn test_as_trait_not_supported() {
    let event = zx::Event::create();
    let cap = TestHandle(event.into());
    let any: AnyCapability = Box::new(cap);

    trait Foo {}
    let foo: Result<&dyn Foo, _> = sandbox::try_as_trait!(Foo, &any);
    assert!(foo.is_err());
}

/// A cloneable capability that holds a string.
#[derive(Capability, Clone, Debug)]
#[capability(as_trait(ReadString))]
struct TestCloneable(pub String);

trait ReadString {
    fn read(&self) -> String;
}

impl ReadString for TestCloneable {
    fn read(&self) -> String {
        self.0.clone()
    }
}

impl Capability for TestCloneable {
    fn to_zx_handle(self) -> (zx::Handle, Option<BoxFuture<'static, ()>>) {
        unimplemented!()
    }

    fn try_clone(&self) -> Result<Self, CloneError> {
        Ok(self.clone())
    }
}

/// Tests that the try_clone impl succeeds.
#[test]
fn test_try_clone() {
    let cap = TestCloneable("hello".to_string());
    let clone = cap.try_clone().expect("failed to clone");
    assert_eq!(clone.0, "hello".to_string());
}

/// Tests that the default try_clone impl returns an error.
#[test]
fn test_try_clone_none() {
    let cap = TestHandle(zx::Handle::invalid());
    assert!(cap.try_clone().is_err());
}

/// Tests that the default try_into_capability impl succeeds in converting to the `Self` type.
#[test]
fn test_convert_to_self_only() {
    let cap = TestCloneable("hello".to_string());
    let converted: Box<dyn Any> = cap.try_into_capability(TypeId::of::<TestCloneable>()).unwrap();
    let cap: TestCloneable = *converted.downcast::<TestCloneable>().unwrap();
    assert_eq!(cap.0, "hello".to_string());
}

/// Tests that the default try_into_capability impl returns an error when trying to convert to a
/// non-`Self` type.
#[test]
fn test_convert_to_self_only_wrong_type() {
    let cap = TestCloneable("hello".to_string());
    let convert_result = cap.try_into_capability(TypeId::of::<TestHandle>());
    // TestCloneable can only be converted to itself, not to TestHandle.
    assert!(convert_result.is_err());
}

#[test]
fn test_as_trait_supported() {
    let cap = TestCloneable("hello".to_string());
    let any: AnyCapability = Box::new(cap);
    let read_string: &dyn ReadString = sandbox::try_as_trait!(ReadString, &any)
        .expect("TestCloneable should implement ReadString");
    assert_eq!(read_string.read().as_str(), "hello");
}

/// Tests the `TryFrom<&AnyCapability>` to reference downcast conversion.
#[test]
fn try_from_any_ref() {
    let cap = TestHandle(zx::Handle::invalid());
    let any: AnyCapability = Box::new(cap);

    let from: &AnyCapability = &any;
    let to: &TestHandle = from.try_into().unwrap();

    assert!(to.0.is_invalid());
}

/// Tests the `TryFrom<&mut AnyCapability>` to mut reference downcast conversion.
#[test]
fn try_from_any_mut_ref() {
    let cap = TestHandle(zx::Handle::invalid());
    let mut any: AnyCapability = Box::new(cap);

    let from: &mut AnyCapability = &mut any;
    let to: &mut TestHandle = from.try_into().unwrap();

    assert!(to.0.is_invalid());
}

/// Tests the `TryFrom<&dyn ErasedCapability>` to reference downcast conversion.
#[test]
fn try_from_dyn_erased_ref() {
    let cap = TestHandle(zx::Handle::invalid());
    let any: AnyCapability = Box::new(cap);

    let from: &dyn ErasedCapability = any.as_ref();
    let to: &TestHandle = from.try_into().unwrap();

    assert!(to.0.is_invalid());
}

/// Tests the `TryFrom<&mut dyn ErasedCapability>` to mut reference downcast conversion.
#[test]
fn try_from_dyn_erased_mut_ref() {
    let cap = TestHandle(zx::Handle::invalid());
    let mut any: AnyCapability = Box::new(cap);

    let from: &mut dyn ErasedCapability = any.borrow_mut();
    let to: &mut TestHandle = from.try_into().unwrap();

    assert!(to.0.is_invalid());
}

/// A capability that implements many traits.
#[derive(Capability, Clone, Debug)]
#[capability(as_trait(GetInt, GetStr))]
struct TestManyTrait;

impl Capability for TestManyTrait {
    fn to_zx_handle(self) -> (zx::Handle, Option<BoxFuture<'static, ()>>) {
        unimplemented!()
    }

    fn try_clone(&self) -> Result<Self, CloneError> {
        Ok(self.clone())
    }
}

trait GetInt {
    fn get_int(&self) -> i32 {
        42
    }
}

trait GetStr {
    fn get_str(&self) -> &'static str {
        "abc"
    }
}

impl GetInt for TestManyTrait {}
impl GetStr for TestManyTrait {}

#[test]
fn test_try_as_different_traits() {
    let cap = TestManyTrait;
    let any: AnyCapability = Box::new(cap);
    let get_int = sandbox::try_as_trait!(GetInt, &any).unwrap();
    assert_eq!(get_int.get_int(), 42);
    let get_str = sandbox::try_as_trait!(GetStr, &any).unwrap();
    assert_eq!(get_str.get_str(), "abc");
}
