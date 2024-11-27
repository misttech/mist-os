// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use anyhow::{anyhow, Result};
use fidl::endpoints::create_proxy;
use fuchsia_inspect::Property;
use futures::future::{FutureExt, LocalBoxFuture};
use std::sync::Arc;
use zx::{HandleBased, Rights};
use {fidl_fuchsia_power_broker as fbroker, fuchsia_async as fasync};

/// A well-known set of PowerLevels to be specified as the valid_levels for a
/// power element. This is the set of levels in fbroker::BinaryPowerLevel.
pub const BINARY_POWER_LEVELS: [fbroker::PowerLevel; 2] = [
    fbroker::BinaryPowerLevel::Off.into_primitive(),
    fbroker::BinaryPowerLevel::On.into_primitive(),
];

pub struct PowerElementContext {
    pub element_control: fbroker::ElementControlProxy,
    pub lessor: fbroker::LessorProxy,
    pub required_level: fbroker::RequiredLevelProxy,
    pub current_level: fbroker::CurrentLevelProxy,
    assertive_dependency_token: Option<fbroker::DependencyToken>,
    opportunistic_dependency_token: Option<fbroker::DependencyToken>,
    name: String,
}

impl PowerElementContext {
    pub fn builder<'a>(
        topology: &'a fbroker::TopologyProxy,
        element_name: &'a str,
        valid_levels: &'a [fbroker::PowerLevel],
    ) -> PowerElementContextBuilder<'a> {
        PowerElementContextBuilder::new(topology, element_name, valid_levels)
    }

    pub fn assertive_dependency_token(&self) -> Option<fbroker::DependencyToken> {
        self.assertive_dependency_token.as_ref().and_then(|token| {
            Some(token.duplicate_handle(Rights::SAME_RIGHTS).expect("failed to duplicate token"))
        })
    }

    pub fn opportunistic_dependency_token(&self) -> Option<fbroker::DependencyToken> {
        self.opportunistic_dependency_token.as_ref().and_then(|token| {
            Some(token.duplicate_handle(Rights::SAME_RIGHTS).expect("failed to duplicate token"))
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

pub struct PowerElementContextBuilder<'a> {
    topology: &'a fbroker::TopologyProxy,
    element_name: &'a str,
    initial_current_level: fbroker::PowerLevel,
    valid_levels: &'a [fbroker::PowerLevel],
    dependencies: Vec<fbroker::LevelDependency>,
    register_dependency_tokens: bool,
}

impl<'a> PowerElementContextBuilder<'a> {
    pub fn new(
        topology: &'a fbroker::TopologyProxy,
        element_name: &'a str,
        valid_levels: &'a [fbroker::PowerLevel],
    ) -> Self {
        Self {
            topology,
            element_name,
            valid_levels,
            initial_current_level: Default::default(),
            dependencies: Default::default(),
            register_dependency_tokens: true,
        }
    }

    pub fn initial_current_level(mut self, value: fbroker::PowerLevel) -> Self {
        self.initial_current_level = value;
        self
    }

    pub fn dependencies(mut self, value: Vec<fbroker::LevelDependency>) -> Self {
        self.dependencies = value;
        self
    }

    pub fn register_dependency_tokens(mut self, enable: bool) -> Self {
        self.register_dependency_tokens = enable;
        self
    }

    pub async fn build(self) -> Result<PowerElementContext> {
        let (current_level, current_level_server_end) =
            create_proxy::<fbroker::CurrentLevelMarker>();
        let (required_level, required_level_server_end) =
            create_proxy::<fbroker::RequiredLevelMarker>();
        let (lessor, lessor_server_end) = create_proxy::<fbroker::LessorMarker>();
        let (element_control, element_control_server_end) =
            create_proxy::<fbroker::ElementControlMarker>();
        self.topology
            .add_element(fbroker::ElementSchema {
                element_name: Some(self.element_name.into()),
                initial_current_level: Some(self.initial_current_level),
                valid_levels: Some(self.valid_levels.to_vec()),
                dependencies: Some(self.dependencies),
                level_control_channels: Some(fbroker::LevelControlChannels {
                    current: current_level_server_end,
                    required: required_level_server_end,
                }),
                lessor_channel: Some(lessor_server_end),
                element_control: Some(element_control_server_end),
                ..Default::default()
            })
            .await?
            .map_err(|d| anyhow::anyhow!("{d:?}"))?;

        let assertive_dependency_token = match self.register_dependency_tokens {
            true => {
                let token = fbroker::DependencyToken::create();
                let _ = element_control
                    .register_dependency_token(
                        token
                            .duplicate_handle(Rights::SAME_RIGHTS)
                            .expect("failed to duplicate token"),
                        fbroker::DependencyType::Assertive,
                    )
                    .await?
                    .expect("register assertive dependency token");
                Some(token)
            }
            false => None,
        };

        let opportunistic_dependency_token = match self.register_dependency_tokens {
            true => {
                let token = fbroker::DependencyToken::create();
                let _ = element_control
                    .register_dependency_token(
                        token
                            .duplicate_handle(Rights::SAME_RIGHTS)
                            .expect("failed to duplicate token"),
                        fbroker::DependencyType::Opportunistic,
                    )
                    .await?
                    .expect("register opportunistic dependency token");
                Some(token)
            }
            false => None,
        };

        Ok(PowerElementContext {
            element_control,
            lessor,
            required_level,
            current_level,
            assertive_dependency_token,
            opportunistic_dependency_token,
            name: self.element_name.to_string(),
        })
    }
}

/// Creates an update function for run_power_element that only updates `power_element``.
///
/// This helper function can be used to create an update function that has no side effects or
/// conditions when the power level of the given power element changes.
pub fn basic_update_fn_factory<'a>(
    power_element: &'a PowerElementContext,
) -> Box<dyn Fn(fbroker::PowerLevel) -> LocalBoxFuture<'a, ()> + 'a> {
    Box::new(move |new_power_level: fbroker::PowerLevel| {
        async move {
            let element_name = power_element.name();

            tracing::debug!(
                ?element_name,
                ?new_power_level,
                "basic_update_fn_factory: updating current level"
            );

            let res = power_element.current_level.update(new_power_level).await;
            if let Err(error) = res {
                tracing::warn!(
                    ?element_name,
                    ?error,
                    "basic_update_fn_factory: updating current level failed"
                );
            }
        }
        .boxed_local()
    })
}

/// Runs a procedure that calls an update function when the required power level changes.
///
/// The power element's power level is expected to be updated in `update_fn``.
/// A minimal update function can be created by calling `basic_update_fn_factory`.
pub async fn run_power_element<'a>(
    element_name: &'a str,
    required_level_proxy: &'a fbroker::RequiredLevelProxy,
    initial_level: fbroker::PowerLevel,
    inspect_node: Option<fuchsia_inspect::Node>,
    update_fn: Box<dyn Fn(fbroker::PowerLevel) -> LocalBoxFuture<'a, ()> + 'a>,
) {
    let mut last_required_level = initial_level;
    let power_level_node = inspect_node
        .as_ref()
        .map(|node| node.create_uint("power_level", last_required_level.into()));

    loop {
        tracing::debug!(
            ?element_name,
            ?last_required_level,
            "run_power_element: waiting for new level"
        );
        match required_level_proxy.watch().await {
            Ok(Ok(required_level)) => {
                tracing::debug!(
                    ?element_name,
                    ?required_level,
                    ?last_required_level,
                    "run_power_element: new level requested"
                );
                if required_level == last_required_level {
                    tracing::debug!(
                        ?element_name,
                        ?required_level,
                        ?last_required_level,
                        "run_power_element: required level has not changed, skipping."
                    );
                    continue;
                }

                update_fn(required_level).await;
                if let Some(ref power_level_node) = power_level_node {
                    power_level_node.set(required_level.into());
                }
                last_required_level = required_level;
            }
            error => {
                tracing::warn!(
                    ?element_name,
                    ?error,
                    "run_power_element: watch_required_level failed"
                );
                return;
            }
        }
    }
}

/// A dependency for a lease. It is equivalent to an fbroker::LevelDependency with the dependent
/// fields omitted.
pub struct LeaseDependency {
    pub dependency_type: fbroker::DependencyType,
    pub requires_token: fbroker::DependencyToken,
    pub requires_level_by_preference: Vec<fbroker::PowerLevel>,
}

/// Helper for acquiring leases. Instantiate with LeaseControl::new(), and then acquire a lease with
/// the lease() method. The lease() call will return only once the lease is satisfied.
///
/// A single LeaseHelper may be reused to create leases an arbitrary number of times.
pub struct LeaseHelper {
    lessor: fbroker::LessorProxy,
    _element_runner: fasync::Task<()>,
}

pub struct Lease {
    /// This may be used to further monitor the lease status, if desired, beyond the
    /// await-until-satisfied behavior of LeaseHelper::lease().
    pub control_proxy: fbroker::LeaseControlProxy,

    // The originating LeaseHelper must be kept alive as long as the lease to keep its associated
    // power element running.
    _helper: Arc<LeaseHelper>,
}

impl LeaseHelper {
    /// Creates a new LeaseHelper. Returns an error upon failure to register the to-be-leased power
    /// element with Power Broker.
    pub async fn new<'a>(
        topology: &'a fbroker::TopologyProxy,
        name: &'a str,
        lease_dependencies: Vec<LeaseDependency>,
    ) -> Result<Arc<Self>> {
        let level_dependencies = lease_dependencies
            .into_iter()
            .map(|d| fbroker::LevelDependency {
                dependency_type: d.dependency_type,
                dependent_level: BINARY_POWER_LEVELS[1],
                requires_token: d.requires_token,
                requires_level_by_preference: d.requires_level_by_preference,
            })
            .collect();

        let element_context = PowerElementContext::builder(topology, name, &BINARY_POWER_LEVELS)
            .dependencies(level_dependencies)
            .initial_current_level(BINARY_POWER_LEVELS[0])
            .build()
            .await?;

        let lessor = element_context.lessor.clone();

        let _element_runner = fasync::Task::local(async move {
            run_power_element(
                &element_context.name(),
                &element_context.required_level,
                BINARY_POWER_LEVELS[0], /* initial_level */
                None,                   /* inspect_node */
                basic_update_fn_factory(&element_context),
            )
            .await;
        });

        Ok(Arc::new(Self { lessor, _element_runner }))
    }

    /// Acquires a lease, completing only once the lease is satisfied. Returns an error if the
    /// underlying `Lessor.Lease` or `LeaseControl.WatchStatus` call fails.
    pub async fn lease(self: &Arc<Self>) -> Result<Lease> {
        let lease = self
            .lessor
            .lease(BINARY_POWER_LEVELS[1])
            .await?
            .map_err(|e| anyhow!("PowerBroker::LeaseError({e:?})"))?;

        // Wait for the lease to be satisfied.
        let lease = lease.into_proxy();
        let mut status = fbroker::LeaseStatus::Unknown;
        loop {
            match lease.watch_status(status).await? {
                fbroker::LeaseStatus::Satisfied => break,
                new_status @ _ => status = new_status,
            }
        }

        Ok(Lease { control_proxy: lease, _helper: self.clone() })
    }
}

// TODO(https://fxbug.dev/349841776): Use this as a demo case for test library support for faking
// Power Broker interfaces.
#[cfg(test)]
mod tests {
    use super::*;
    use diagnostics_assertions::assert_data_tree;
    use fuchsia_async as fasync;
    use futures::channel::mpsc;
    use futures::StreamExt;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn run_required_level_server(
        mut required_power_levels: Vec<fbroker::PowerLevel>,
    ) -> fbroker::RequiredLevelProxy {
        let (required_level_proxy, mut required_level_stream) =
            fidl::endpoints::create_proxy_and_stream::<fbroker::RequiredLevelMarker>().unwrap();

        fasync::Task::local(async move {
            while let Some(Ok(request)) = required_level_stream.next().await {
                match request {
                    fbroker::RequiredLevelRequest::Watch { responder } => {
                        responder
                            .send(
                                required_power_levels
                                    .pop()
                                    .ok_or_else(|| fbroker::RequiredLevelError::Internal),
                            )
                            .unwrap();
                    }
                    _ => unreachable!("Unexpected method call"),
                }
            }
        })
        .detach();

        required_level_proxy
    }

    #[fuchsia::test]
    async fn basic_update_fn_factory_performs_update() -> Result<()> {
        let (element_control, _element_control_stream) =
            fidl::endpoints::create_proxy_and_stream::<fbroker::ElementControlMarker>()?;
        let (lessor, _lessor_stream) =
            fidl::endpoints::create_proxy_and_stream::<fbroker::LessorMarker>()?;
        let (required_level, _required_level_stream) =
            fidl::endpoints::create_proxy_and_stream::<fbroker::RequiredLevelMarker>()?;
        let (current_level, mut current_level_stream) =
            fidl::endpoints::create_proxy_and_stream::<fbroker::CurrentLevelMarker>()?;

        let power_element = PowerElementContext {
            element_control,
            lessor,
            required_level,
            current_level,
            assertive_dependency_token: Some(fbroker::DependencyToken::create()),
            opportunistic_dependency_token: Some(fbroker::DependencyToken::create()),
            name: "test_name".to_string(),
        };

        fasync::Task::local(async move {
            let update_fn = basic_update_fn_factory(&power_element);
            update_fn(100).await;
        })
        .detach();

        let Some(Ok(fbroker::CurrentLevelRequest::Update { current_level, responder })) =
            current_level_stream.next().await
        else {
            unreachable!();
        };

        responder.send(Ok(())).unwrap();
        assert_eq!(100, current_level);
        Ok(())
    }

    #[fuchsia::test]
    async fn run_power_element_passes_required_level_to_update_fn() -> Result<()> {
        let (tx, mut rx) = mpsc::channel(5);
        let required_level_proxy = run_required_level_server(vec![1, 2]);

        run_power_element(
            "test_element",
            &required_level_proxy,
            0,
            None,
            Box::new(|power_level| {
                let mut tx = tx.clone();
                async move {
                    tx.start_send(power_level).unwrap();
                }
                .boxed_local()
            }),
        )
        .await;

        assert_eq!(2, rx.next().await.unwrap());
        assert_eq!(1, rx.next().await.unwrap());
        Ok(())
    }

    #[fuchsia::test]
    async fn run_power_element_skips_update_on_same_level() -> Result<()> {
        let (tx, mut rx) = mpsc::channel(5);
        let initial_level = 5;
        let required_level_proxy = run_required_level_server(vec![3, 1, 1, 2, 2, initial_level]);

        run_power_element(
            "test_element",
            &required_level_proxy,
            initial_level,
            None,
            Box::new(|power_level| {
                let mut tx = tx.clone();
                async move {
                    tx.start_send(power_level).unwrap();
                }
                .boxed_local()
            }),
        )
        .await;

        assert_eq!(2, rx.next().await.unwrap());
        assert_eq!(1, rx.next().await.unwrap());
        assert_eq!(3, rx.next().await.unwrap());
        Ok(())
    }

    #[fuchsia::test]
    async fn run_power_element_updates_inspect_node() -> Result<()> {
        let inspector = fuchsia_inspect::Inspector::default();
        let (mut tx, rx) = mpsc::channel(5);
        let (tx2, mut rx2) = mpsc::channel(5);
        let rx = Rc::new(RefCell::new(rx));
        let required_level_proxy = run_required_level_server(vec![1, 4, 0, 3]);

        let root = inspector.root().clone_weak();
        fasync::Task::local(async move {
            run_power_element(
                "test_element",
                &required_level_proxy,
                0,
                Some(root),
                Box::new(|_| {
                    let rx = rx.clone();
                    let mut tx2 = tx2.clone();
                    async move {
                        tx2.start_send(()).unwrap();
                        rx.borrow_mut().next().await.unwrap();
                    }
                    .boxed_local()
                }),
            )
            .await;
        })
        .detach();

        // The first communication hasn't updated the tree yet.
        rx2.next().await.unwrap();
        tx.start_send(()).unwrap();

        // Now that the update function has been called twice, the inspect tree
        // should show the first power level. This pattern continues
        rx2.next().await.unwrap();
        assert_data_tree!(inspector, root: {
            power_level: 3u64
        });
        tx.start_send(()).unwrap();

        rx2.next().await.unwrap();
        assert_data_tree!(inspector, root: {
            power_level: 0u64
        });
        tx.start_send(()).unwrap();

        rx2.next().await.unwrap();
        assert_data_tree!(inspector, root: {
            power_level: 4u64
        });
        Ok(())
    }
}
