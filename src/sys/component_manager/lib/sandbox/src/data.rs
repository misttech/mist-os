// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use crate::{any::ErasedCapability, AnyCapability, AnyCast, Capability, Remote};
use fuchsia_zircon as zx;
use std::borrow::BorrowMut;
use std::fmt::Debug;

#[derive(Capability, Debug, Clone, Default)]
#[capability(try_clone = "clone", convert = "to_self_only")]
pub struct Data<T: Debug + Clone + Send + Sync + 'static> {
    pub value: T,
}

impl<T: Debug + Clone + Send + Sync + 'static> Data<T> {
    pub fn new(value: T) -> Self {
        Self { value }
    }
}

impl<T: Debug + Clone + Send + Sync + 'static> Remote for Data<T> {
    fn to_zx_handle(self) -> (zx::Handle, Option<futures::future::BoxFuture<'static, ()>>) {
        todo!("we may want to expose a FIDL or VMO to read and write data")
    }
}

impl<'a, T: Debug + Clone + Send + Sync + 'static> TryFrom<&'a dyn ErasedCapability>
    for &'a Data<T>
{
    type Error = ();

    fn try_from(value: &dyn ErasedCapability) -> Result<&Data<T>, ()> {
        value.as_any().downcast_ref::<Data<T>>().ok_or(())
    }
}

impl<'a, T: Debug + Clone + Send + Sync + 'static> TryFrom<&'a mut dyn ErasedCapability>
    for &'a mut Data<T>
{
    type Error = ();

    fn try_from(value: &mut dyn ErasedCapability) -> Result<&mut Data<T>, ()> {
        value.as_any_mut().downcast_mut::<Data<T>>().ok_or(())
    }
}

impl<'a, T: Debug + Clone + Send + Sync + 'static> TryFrom<&'a AnyCapability> for &'a Data<T> {
    type Error = ();

    fn try_from(value: &AnyCapability) -> Result<&Data<T>, ()> {
        value.as_ref().try_into()
    }
}

impl<'a, T: Debug + Clone + Send + Sync + 'static> TryFrom<&'a mut AnyCapability>
    for &'a mut Data<T>
{
    type Error = ();

    fn try_from(value: &mut AnyCapability) -> Result<&mut Data<T>, ()> {
        let borrowed: &mut dyn ErasedCapability = value.borrow_mut();
        borrowed.try_into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TryClone;

    #[test]
    fn try_from_any_into_self() {
        let data: Data<i32> = Data::new(1);
        let any: AnyCapability = Box::new(data);
        let data_back: Data<i32> = any.try_into().unwrap();
        assert_eq!(data_back.value, 1);

        let data: Data<String> = Data::new("abc".to_string());
        let any: AnyCapability = Box::new(data);
        let data_back: Data<String> = any.try_into().unwrap();
        assert_eq!(data_back.value, "abc");
    }

    #[test]
    fn try_from_any_into_wrong_value_type_fails() {
        let data: Data<i32> = Data::new(1);
        let any: AnyCapability = Box::new(data);
        let value_result: Result<Data<i64>, _> = any.try_into();
        assert!(value_result.is_err());
    }

    #[test]
    fn try_from_any_into_ref() {
        let data: Data<i32> = Data::new(1);
        let any: AnyCapability = Box::new(data);
        let data_back: &Data<i32> = (&any).try_into().unwrap();
        assert_eq!(data_back.value, 1);

        let data: Data<String> = Data::new("abc".to_string());
        let any: AnyCapability = Box::new(data);
        let data_back: &Data<String> = (&any).try_into().unwrap();
        assert_eq!(data_back.value, "abc");
    }

    #[test]
    fn try_clone() {
        let data: Data<String> = Data::new("abc".to_string());
        let any: AnyCapability = Box::new(data);
        let clone = any.try_clone().unwrap();
        let data_back: Data<String> = any.try_into().unwrap();
        let clone_data_back: Data<String> = clone.try_into().unwrap();
        assert_eq!(data_back.value, clone_data_back.value);
    }
}
