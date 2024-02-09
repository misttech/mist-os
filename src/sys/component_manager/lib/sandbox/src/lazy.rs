// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_component_sandbox as fsandbox;
use futures::{future::BoxFuture, FutureExt};
use std::sync::Arc;

use crate::{AnyCapability, Capability};

/// A "promise" for one or more capabilities that are resolved asynchronously.
///
/// The inner function that generates the capability can be called multiple times to produce
/// multiple capabilities for a single Lazy. Clones of Lazy call the same function.
#[derive(Capability, Clone)]
pub struct Lazy(Arc<dyn Fn() -> BoxFuture<'static, Result<AnyCapability, ()>> + Send + Sync>);

impl Lazy {
    pub fn new<F>(func: F) -> Self
    where
        F: Fn() -> BoxFuture<'static, Result<AnyCapability, ()>> + Send + Sync + 'static,
    {
        Self(Arc::new(func))
    }

    /// Call the function to get a future for the capability.
    pub fn get(&self) -> BoxFuture<'static, Result<AnyCapability, ()>> {
        self.0()
    }

    /// Maps this Lazy's capability to a new capability, returning a new Lazy.
    ///
    /// The function is applied on the `Ok` value of the result of calling `get` on the Lazy,
    /// and the `Err` value is returned as-is.
    pub fn map<F>(self, func: F) -> Self
    where
        F: Fn(AnyCapability) -> BoxFuture<'static, Result<AnyCapability, ()>>
            + Send
            + Sync
            + 'static,
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

impl Capability for Lazy {}

impl From<Lazy> for fsandbox::Capability {
    fn from(_lazy: Lazy) -> Self {
        todo!("https://fxbug.dev/314848350: Implement the conversion from sandbox::Lazy to FIDL")
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{Data, Unit};
    use futures::FutureExt;

    #[fuchsia::test]
    async fn test_lazy() {
        let lazy = Lazy::new(|| {
            async {
                let cap: AnyCapability = Box::new(Unit::default());
                Ok(cap)
            }
            .boxed()
        });

        // Resolve the lazy to get a Unit capability.
        let cap = lazy.get().await.unwrap();
        let unit: Unit = cap.try_into().unwrap();
        assert_eq!(unit, Unit::default());

        // Resolve it again to get another Unit capability.
        let cap = lazy.get().await.unwrap();
        let unit: Unit = cap.try_into().unwrap();
        assert_eq!(unit, Unit::default());
    }

    #[fuchsia::test]
    async fn test_lazy_clone() {
        let lazy = Lazy::new(|| {
            Box::pin(async {
                let cap: AnyCapability = Box::new(Unit::default());
                Ok(cap)
            })
        });
        let clone = lazy.clone();

        let cap = lazy.get().await.unwrap();
        let unit: Unit = cap.try_into().unwrap();
        assert_eq!(unit, Unit::default());

        let clone_cap = clone.get().await.unwrap();
        let clone_unit: Unit = clone_cap.try_into().unwrap();
        assert_eq!(clone_unit, Unit::default());
    }

    #[fuchsia::test]
    async fn test_lazy_error() {
        let lazy = Lazy::new(|| Box::pin(async { Err(()) }));
        assert!(lazy.get().await.is_err());
    }

    #[fuchsia::test]
    async fn test_lazy_map() {
        let lazy = Lazy::new(|| {
            async {
                let cap: AnyCapability = Box::new(Unit::default());
                Ok(cap)
            }
            .boxed()
        });

        // Map the Lazy into a new Lazy that returns a Data instead of Unit.
        let mapped = lazy.map(|cap| {
            async move {
                let unit: Unit = cap.try_into().unwrap();
                assert_eq!(unit, Unit::default());

                // Return a Data instead of Unit.
                Ok(Box::new(Data::String("hello".to_string())) as AnyCapability)
            }
            .boxed()
        });

        let cap = mapped.get().await.unwrap();
        let data: Data = cap.try_into().unwrap();
        assert_eq!(data, Data::String("hello".to_string()));
    }
}
