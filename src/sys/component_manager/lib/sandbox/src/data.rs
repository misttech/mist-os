// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use crate::{AnyCast, Capability, CloneError};
use fuchsia_zircon as zx;
use std::fmt::Debug;

#[derive(Capability, Debug, Clone, Default)]
pub struct Data<T: Debug + Clone + Send + Sync + 'static> {
    pub value: T,
}

impl<T: Debug + Clone + Send + Sync + 'static> Data<T> {
    pub fn new(value: T) -> Self {
        Self { value }
    }
}

impl<T: Debug + Clone + Send + Sync + 'static> Capability for Data<T> {
    fn try_clone(&self) -> Result<Self, CloneError> {
        Ok(self.clone())
    }

    fn to_zx_handle(self) -> (zx::Handle, Option<futures::future::BoxFuture<'static, ()>>) {
        todo!("we may want to expose a FIDL or VMO to read and write data")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AnyCapability;

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
