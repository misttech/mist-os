// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{AnyCapability, AnyCloneCapability, Capability, Remote, TryIntoOpen};
use std::fmt::Debug;

#[derive(Debug, Clone)]
pub struct Data<T> {
    pub value: T,
}

impl<T> Remote for Data<T> {
    fn to_zx_handle(
        self: Box<Self>,
    ) -> (fuchsia_zircon::Handle, Option<futures::future::BoxFuture<'static, ()>>) {
        todo!("we may want to expose a FIDL or VMO to read and write data")
    }
}

impl<T> TryIntoOpen for Data<T> {}

impl<T> Capability for Data<T> where T: Debug + Send + Sync + 'static {}

pub trait AsData {
    fn as_data<T>(self: &Self) -> Option<&Data<T>>
    where
        T: Debug + Send + Sync + 'static;
}

impl AsData for AnyCapability {
    fn as_data<T>(self: &Self) -> Option<&Data<T>>
    where
        T: Debug + Send + Sync + 'static,
    {
        self.as_any().downcast_ref::<Data<T>>()
    }
}

impl AsData for AnyCloneCapability {
    fn as_data<T>(self: &Self) -> Option<&Data<T>>
    where
        T: Debug + Send + Sync + 'static,
    {
        self.as_any().downcast_ref::<Data<T>>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AnyCapability, AnyCloneCapability, AsData};
    use assert_matches::assert_matches;

    #[test]
    fn test_int_data() {
        let data = Data { value: 1 };
        let cap: AnyCapability = Box::new(data);
        let data_back: Option<&Data<i32>> = cap.as_data();
        assert_eq!(data_back.unwrap().value, 1);
        let data_back = cap.as_data::<i32>();
        assert_eq!(data_back.unwrap().value, 1);
        let wrong: Option<&Data<u64>> = cap.as_data();
        assert_matches!(wrong, None);
    }

    #[test]
    fn test_str_data() {
        let data = Data { value: "abc".to_string() };
        let cap: AnyCloneCapability = Box::new(data);
        let data_back: Option<&Data<String>> = cap.as_data();
        assert_eq!(data_back.unwrap().value, "abc");
        let data_back = cap.as_data::<String>();
        assert_eq!(data_back.unwrap().value, "abc");
        let clone = cap.clone();
        let data_back: Option<&Data<String>> = clone.as_data();
        assert_eq!(data_back.unwrap().value, "abc");
        let wrong: Option<&Data<&str>> = cap.as_data();
        assert_matches!(wrong, None);
    }
}
