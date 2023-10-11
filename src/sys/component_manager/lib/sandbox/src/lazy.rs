// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{AnyCapability, Capability, CloneError};
use anyhow::Result;
use fuchsia_zircon as zx;
use futures::{future::BoxFuture, FutureExt};
use std::sync::Arc;

/// A "promise" for one or more capabilities that are resolved asynchronously.
///
/// The inner function that generates the capability can be called multiple times to produce
/// multiple capabilities for a single Lazy. This means the Lazy is cloneable, and clones call
/// the same function.
#[derive(Capability, Clone)]
pub struct Lazy(Arc<dyn Fn() -> BoxFuture<'static, Result<AnyCapability>> + Send + Sync>);

impl Lazy {
    pub fn new<F>(func: F) -> Self
    where
        F: Fn() -> BoxFuture<'static, Result<AnyCapability>> + Send + Sync + 'static,
    {
        Self(Arc::new(func))
    }

    /// Call the function to get a future for the capability.
    pub fn get(&self) -> BoxFuture<'static, Result<AnyCapability>> {
        self.0()
    }

    /// Maps this Lazy's capability to a new capability, returning a new Lazy.
    ///
    /// The function is applied on the `Ok` value of the result of calling `get` on the Lazy,
    /// and the `Err` value is returned as-is.
    pub fn map<F>(self, func: F) -> Self
    where
        F: Fn(AnyCapability) -> BoxFuture<'static, Result<AnyCapability>> + Send + Sync + 'static,
    {
        let func = Arc::new(func);
        Self::new(move || {
            let self_ = self.clone();
            let func = func.clone();
            async move {
                let cap_result = self_.get().await?;
                func(cap_result).await
            }
            .boxed()
        })
    }
}

impl std::fmt::Debug for Lazy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lazy").finish()
    }
}

impl Capability for Lazy {
    fn try_clone(&self) -> Result<Self, CloneError> {
        Ok(self.clone())
    }

    fn to_zx_handle(self) -> (zx::Handle, Option<BoxFuture<'static, ()>>) {
        todo!("TODO(fxbug.dev/298112397): Implement to_zx_handle for Lazy")
    }
}

/// A "promise" for a single capability that is resolved asynchronously.
///
/// The inner function that generates the capability can only be called once, consuming
/// the LazyOnce.
#[derive(Capability)]
pub struct LazyOnce(Box<dyn FnOnce() -> BoxFuture<'static, Result<AnyCapability>> + Send + Sync>);

impl LazyOnce {
    pub fn new<F>(func: F) -> Self
    where
        F: FnOnce() -> BoxFuture<'static, Result<AnyCapability>> + Send + Sync + 'static,
    {
        Self(Box::new(func))
    }

    /// Call the function to get a future for the capability.
    pub fn get(self) -> BoxFuture<'static, Result<AnyCapability>> {
        self.0()
    }
}

impl std::fmt::Debug for LazyOnce {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LazyOnce").finish()
    }
}

impl Capability for LazyOnce {
    fn to_zx_handle(self) -> (zx::Handle, Option<BoxFuture<'static, ()>>) {
        todo!("TODO(fxbug.dev/298112397): Implement to_zx_handle for LazyOnce")
    }
}

#[cfg(test)]
mod test {
    use super::{Lazy, LazyOnce};
    use anyhow::anyhow;
    use futures::FutureExt;
    use sandbox::{AnyCapability, Data};

    #[fuchsia::test]
    async fn test_lazy() {
        let lazy = Lazy::new(|| {
            async {
                let cap: AnyCapability = Box::new(Data::new("hello".to_string()));
                Ok(cap)
            }
            .boxed()
        });

        // Resolve the lazy to get a Data capability.
        let cap = lazy.get().await.unwrap();
        let data: Data<String> = cap.try_into().unwrap();
        assert_eq!(data.value, "hello");

        // Resolve it again to get another Data capability.
        let cap = lazy.get().await.unwrap();
        let data: Data<String> = cap.try_into().unwrap();
        assert_eq!(data.value, "hello");
    }

    #[fuchsia::test]
    async fn test_lazy_clone() {
        let lazy = Lazy::new(|| {
            Box::pin(async {
                let cap: AnyCapability = Box::new(Data::new("hello".to_string()));
                Ok(cap)
            })
        });
        let clone = lazy.clone();

        let cap = lazy.get().await.unwrap();
        let data: Data<String> = cap.try_into().unwrap();
        assert_eq!(data.value, "hello");

        let clone_cap = clone.get().await.unwrap();
        let clone_data: Data<String> = clone_cap.try_into().unwrap();
        assert_eq!(clone_data.value, "hello");
    }

    #[fuchsia::test]
    async fn test_lazy_error() {
        let lazy = Lazy::new(|| Box::pin(async { Err(anyhow!("some error")) }));
        assert!(lazy.get().await.is_err());
    }

    #[fuchsia::test]
    async fn test_lazy_map() {
        let lazy = Lazy::new(|| {
            async {
                let cap: AnyCapability = Box::new(Data::new("hello".to_string()));
                Ok(cap)
            }
            .boxed()
        });

        // Map the Lazy into a new Lazy that changes the inner value.
        let mapped = lazy.map(|cap| {
            async move {
                let mut data: Data<String> = cap.try_into().unwrap();
                data.value = "world".to_string();
                Ok(Box::new(data) as AnyCapability)
            }
            .boxed()
        });

        let cap = mapped.get().await.unwrap();
        let data: Data<String> = cap.try_into().unwrap();
        assert_eq!(data.value, "world");
    }

    #[fuchsia::test]
    async fn test_lazyonce() {
        let lazy = LazyOnce::new(|| {
            async {
                let cap: AnyCapability = Box::new(Data::new("hello".to_string()));
                Ok(cap)
            }
            .boxed()
        });

        let cap = lazy.get().await.unwrap();
        let data: Data<String> = cap.try_into().unwrap();
        assert_eq!(data.value, "hello");
    }

    #[fuchsia::test]
    async fn test_lazyonce_error() {
        let lazy = LazyOnce::new(|| async { Err(anyhow!("some error")) }.boxed());
        assert!(lazy.get().await.is_err());
    }
}
