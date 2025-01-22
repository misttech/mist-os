// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Error, Result};
use fuchsia_component_test::{Capability, ChildOptions, RealmBuilder, RealmInstance, Ref, Route};
use power_broker_client::{
    self as client_lib, run_power_element, PowerElementContext, BINARY_POWER_LEVELS,
};
use {fidl_fuchsia_power_broker as fpb, fuchsia_async as fasync};

async fn build_power_broker_realm() -> Result<RealmInstance, Error> {
    let builder = RealmBuilder::new().await?;
    let power_broker = builder
        .add_child("power_broker", "power-broker#meta/power-broker.cm", ChildOptions::new())
        .await?;
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<fpb::TopologyMarker>())
                .from(&power_broker)
                .to(Ref::parent()),
        )
        .await?;
    let realm = builder.build().await?;

    Ok(realm)
}

#[cfg(test)]
mod tests {
    #![allow(unused_imports)]
    use super::*;
    use futures::channel::mpsc;
    use futures::future::FutureExt;
    use futures::{pin_mut, StreamExt};
    use std::sync::Arc;
    use zx::{self as zx, HandleBased};

    const OFF: fpb::PowerLevel = fpb::BinaryPowerLevel::Off.into_primitive();
    const ON: fpb::PowerLevel = fpb::BinaryPowerLevel::On.into_primitive();

    // Creates and runs a binary test element, returning a dependency token and a channel for
    // listening to its level changes.
    async fn create_test_element(
        topology: &fpb::TopologyProxy,
        name: &str,
    ) -> Result<(fpb::DependencyToken, mpsc::UnboundedReceiver<fpb::PowerLevel>, fasync::Task<()>)>
    {
        let provider = Arc::new(
            PowerElementContext::builder(topology, name, &BINARY_POWER_LEVELS).build().await?,
        );

        let (provider_level_sender, provider_level_receiver) = mpsc::unbounded();
        let current_level_proxy = provider.current_level.clone();
        let dep_token = provider.assertive_dependency_token().unwrap();

        let runner_task = fasync::Task::local(async move {
            run_power_element(
                &provider.name(),
                &provider.required_level,
                OFF,
                None,
                Box::new(move |power_level| {
                    let current_level_proxy = current_level_proxy.clone();
                    let provider_level_sender = provider_level_sender.clone();
                    async move {
                        current_level_proxy.update(power_level).await.unwrap().unwrap();
                        provider_level_sender.unbounded_send(power_level).unwrap();
                    }
                    .boxed_local()
                }),
            )
            .await;
        });

        Ok((dep_token, provider_level_receiver, runner_task))
    }

    // Verifies that LeaseHelper affects a single dependency as expected.
    #[fuchsia::test]
    async fn test_lease_helper_one_dependency() -> Result<(), Error> {
        let realm = build_power_broker_realm().await?;
        let topology = realm.root.connect_to_protocol_at_exposed_dir::<fpb::TopologyMarker>()?;

        let (dependency_token, mut provider_level_receiver, _task) =
            create_test_element(&topology, "Provider").await?;

        let dependency = client_lib::LeaseDependency {
            dependency_type: fpb::DependencyType::Assertive,
            requires_token: dependency_token,
            requires_level_by_preference: vec![ON],
        };
        let helper = client_lib::LeaseHelper::new(&topology, "Lease", vec![dependency]).await?;

        // Before acquiring the lease, Provider has not changed its level.
        assert!(provider_level_receiver.try_next().is_err());

        // The lease() call should only return once the lease is satisfied, so the provider element
        // is ON.
        let lease = helper.create_lease_and_wait_until_satisfied().await?;
        assert_eq!(ON, provider_level_receiver.next().await.unwrap());
        assert!(provider_level_receiver.try_next().is_err());

        // After dropping the lease, the provider element reverts to OFF.
        drop(lease);
        assert_eq!(OFF, provider_level_receiver.next().await.unwrap());
        assert!(provider_level_receiver.try_next().is_err());

        // The helper can be reused to create additional leases.
        let lease = helper.create_lease_and_wait_until_satisfied().await?;
        assert_eq!(ON, provider_level_receiver.next().await.unwrap());
        assert!(provider_level_receiver.try_next().is_err());
        drop(lease);
        assert_eq!(OFF, provider_level_receiver.next().await.unwrap());
        assert!(provider_level_receiver.try_next().is_err());

        Ok(())
    }

    // Verifies that LeaseHelper affects two dependencies as expected.
    #[fuchsia::test]
    async fn test_lease_helper_two_dependencies() -> Result<(), Error> {
        let realm = build_power_broker_realm().await?;
        let topology = realm.root.connect_to_protocol_at_exposed_dir::<fpb::TopologyMarker>()?;

        let (dependency_token_1, mut provider_1_receiver, _task) =
            create_test_element(&topology, "Provider 1").await?;
        let (dependency_token_2, mut provider_2_receiver, _task) =
            create_test_element(&topology, "Provider 2").await?;

        let dependencies = vec![
            client_lib::LeaseDependency {
                dependency_type: fpb::DependencyType::Assertive,
                requires_token: dependency_token_1,
                requires_level_by_preference: vec![ON],
            },
            client_lib::LeaseDependency {
                dependency_type: fpb::DependencyType::Assertive,
                requires_token: dependency_token_2,
                requires_level_by_preference: vec![ON],
            },
        ];
        let helper = client_lib::LeaseHelper::new(&topology, "Lease", dependencies).await?;

        // Before acquiring the lease, neither provider has changed its level.
        assert!(provider_1_receiver.try_next().is_err());
        assert!(provider_2_receiver.try_next().is_err());

        // The lease() call should only return once the lease is satisfied, so both provider
        // elements are ON.
        let lease = helper.create_lease_and_wait_until_satisfied().await?;
        assert_eq!(ON, provider_1_receiver.next().await.unwrap());
        assert_eq!(ON, provider_2_receiver.next().await.unwrap());
        assert!(provider_1_receiver.try_next().is_err());
        assert!(provider_2_receiver.try_next().is_err());

        // After dropping the lease, the provider element reverts to OFF.
        drop(lease);
        assert_eq!(OFF, provider_1_receiver.next().await.unwrap());
        assert_eq!(OFF, provider_2_receiver.next().await.unwrap());
        assert!(provider_1_receiver.try_next().is_err());
        assert!(provider_2_receiver.try_next().is_err());

        Ok(())
    }

    // Verifies that the LeaseHelper::lease() only completes once the lease is satisfied.
    #[fuchsia::test]
    async fn test_lease_blocks_until_satisfied() -> Result<(), Error> {
        let realm = build_power_broker_realm().await?;
        let topology = realm.root.connect_to_protocol_at_exposed_dir::<fpb::TopologyMarker>()?;

        let provider = Arc::new(
            PowerElementContext::builder(&topology, "Provider", &BINARY_POWER_LEVELS)
                .build()
                .await?,
        );

        let provider_lock_local = Arc::new(futures::lock::Mutex::new(()));
        let provider_lock = provider_lock_local.clone();
        let current_level_proxy = provider.current_level.clone();
        let dep_token = provider.assertive_dependency_token().unwrap();

        fasync::Task::local(async move {
            run_power_element(
                &provider.name(),
                &provider.required_level,
                OFF,
                None,
                Box::new(move |power_level| {
                    let provider_lock = provider_lock.clone();
                    let current_level_proxy = current_level_proxy.clone();
                    async move {
                        let _ = provider_lock.lock().await;
                        current_level_proxy.update(power_level).await.unwrap().unwrap();
                    }
                    .boxed_local()
                }),
            )
            .await;
        })
        .detach();

        let dependency = client_lib::LeaseDependency {
            dependency_type: fpb::DependencyType::Assertive,
            requires_token: dep_token,
            requires_level_by_preference: vec![ON],
        };

        let helper = client_lib::LeaseHelper::new(&topology, "Lease", vec![dependency]).await?;
        let lease_future = helper.create_lease_and_wait_until_satisfied().fuse();
        let timer = fasync::Timer::new(fasync::MonotonicInstant::after(
            fasync::MonotonicDuration::from_seconds(1),
        ))
        .fuse();
        pin_mut!(lease_future, timer);

        // While we hold the provider lock, it can't reach its required level. The lease cannot be
        // satisfied, so the lease future will not complete.
        let lock = provider_lock_local.lock().await;
        futures::select! {
            _ = lease_future => panic!("lease() should not return yet"),
            _ = timer => {},
        }

        // After dropping the lock, the lease will become satisfied.
        drop(lock);
        assert!(lease_future.await.is_ok());

        Ok(())
    }

    #[fuchsia::test]
    async fn test_leasing_dropped_element_yields_error() -> Result<(), Error> {
        let realm = build_power_broker_realm().await?;
        let topology = realm.root.connect_to_protocol_at_exposed_dir::<fpb::TopologyMarker>()?;

        let (dependency_token, _, _task) = create_test_element(&topology, "Provider").await?;

        drop(_task);

        // Once Power Broker deletes the Provider element (we don't know how long that will take),
        // acquiring a lease will fail.
        loop {
            let dependency = client_lib::LeaseDependency {
                dependency_type: fpb::DependencyType::Assertive,
                requires_token: dependency_token.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap(),
                requires_level_by_preference: vec![ON],
            };

            if client_lib::LeaseHelper::new(&topology, "Lease", vec![dependency]).await.is_err() {
                break;
            }
            fasync::Timer::new(fasync::MonotonicInstant::after(
                fasync::MonotonicDuration::from_seconds(1),
            ))
            .await;
        }

        Ok(())
    }
}
