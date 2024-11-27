// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::collections::HashMap;
use std::fmt::Display;
use std::sync::Arc;

use crate::common::{GlobalPrincipalIdentifier, GlobalPrincipalIdentifierFactory};
use fuchsia_sync::Mutex;
use tracing::error;
use {fidl_fuchsia_component as fcomponent, fidl_fuchsia_memory_attribution as fattribution};

/// An error of the attribution client.
#[derive(Debug)]
pub enum AttributionClientError {
    /// An unknown field is used in the provider's message.
    UnknownField(String),
    /// A mandatory field is missing in the provider's message.
    MissingField(String),
    /// The client has been unable to resolve a component moniker.
    FailToGetMoniker(fcomponent::Error),
    /// A FIDL connection failed.
    ConnectionFailure(fidl::Error),
    /// An error within the Attribution Provider protocol.
    AttributionProtocolError(fattribution::Error),
}

impl Display for AttributionClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AttributionClientError::UnknownField(name) => {
                write!(f, "UnknownField({})", name)
            }
            AttributionClientError::MissingField(name) => {
                write!(f, "MissingField({})", name)
            }
            AttributionClientError::FailToGetMoniker(err) => {
                write!(f, "FailToGetMoniker({:?})", err)
            }
            AttributionClientError::ConnectionFailure(err) => {
                write!(f, "ConnectionFailure({:?})", err)
            }
            AttributionClientError::AttributionProtocolError(err) => {
                write!(f, "AttributionProtocolError({:?})", err)
            }
        }
    }
}

impl From<fidl::Error> for AttributionClientError {
    fn from(err: fidl::Error) -> Self {
        AttributionClientError::ConnectionFailure(err)
    }
}

impl From<fcomponent::Error> for AttributionClientError {
    fn from(err: fcomponent::Error) -> Self {
        AttributionClientError::FailToGetMoniker(err)
    }
}

impl From<fattribution::Error> for AttributionClientError {
    fn from(err: fattribution::Error) -> Self {
        AttributionClientError::AttributionProtocolError(err)
    }
}

/// A principal identifier, provided by an attribution provider. This identifier is only unique
/// locally, for a given attribution provider.
#[derive(PartialEq, Eq, Hash, Clone, Copy, Debug)]
pub struct LocalPrincipalIdentifier(u64);

impl LocalPrincipalIdentifier {
    const SELF_PRINCIPAL_ID: u64 = 0;

    pub fn self_identifier() -> Self {
        LocalPrincipalIdentifier(LocalPrincipalIdentifier::SELF_PRINCIPAL_ID)
    }

    #[cfg(test)]
    pub fn new_for_tests(value: u64) -> Self {
        Self(value)
    }
}

/// User-understandable description of a Principal
#[derive(PartialEq, Eq, Clone, Debug)]
pub enum PrincipalDescription {
    Component(String),
    Part(String),
}

impl PrincipalDescription {
    async fn new(
        description: Option<fattribution::Description>,
        introspector: &fcomponent::IntrospectorProxy,
    ) -> Result<PrincipalDescription, AttributionClientError> {
        match description.ok_or_else(|| {
            AttributionClientError::MissingField("NewPrincipal::description".to_owned())
        })? {
            fattribution::Description::Component(moniker_token) => {
                Ok(PrincipalDescription::Component(introspector.get_moniker(moniker_token).await??))
            }
            fattribution::Description::Part(part_name) => Ok(PrincipalDescription::Part(part_name)),
            fattribution::Description::__SourceBreaking { unknown_ordinal: _ } => {
                Err(AttributionClientError::UnknownField("Description".to_owned()))
            }
        }
    }
}

/// Type of a principal.
#[derive(PartialEq, Eq, Clone, Debug)]
pub enum PrincipalType {
    Runnable,
    Part,
}

impl TryFrom<fattribution::PrincipalType> for PrincipalType {
    type Error = AttributionClientError;

    fn try_from(value: fattribution::PrincipalType) -> Result<Self, Self::Error> {
        match value {
            fattribution::PrincipalType::Runnable => Ok(PrincipalType::Runnable),
            fattribution::PrincipalType::Part => Ok(PrincipalType::Part),
            fattribution::PrincipalType::__SourceBreaking { unknown_ordinal: _ } => {
                Err(AttributionClientError::UnknownField("PrincipalType".to_owned()))
            }
        }
    }
}

/// Definition of a Principal.
#[derive(PartialEq, Eq, Clone, Debug)]
pub struct PrincipalDefinition {
    /// Parent principal in the AttributionProvider hierarchy, responsible for providing
    /// attribution information for this principal.
    pub attributor: Option<GlobalPrincipalIdentifier>,
    pub id: GlobalPrincipalIdentifier,
    pub description: PrincipalDescription,
    pub principal_type: PrincipalType,
}

/// AttributionProvider holds the attribution state of a single Principal providing attribution
/// claims.
/// [LocalPrincipalIdentifier] should be unique within this [AttributionProvider].
#[derive(Clone)]
pub struct AttributionProvider {
    /// Map from principal ids to principals defined
    pub definitions: HashMap<LocalPrincipalIdentifier, PrincipalDefinition>,
    /// Map from the principal to its attributed resources.
    pub resources: HashMap<LocalPrincipalIdentifier, Vec<fattribution::Resource>>,
}

/// AttributionStateManager stores the state of the published attribution claims.
#[derive(Default, Clone)]
pub struct AttributionState(pub HashMap<GlobalPrincipalIdentifier, AttributionProvider>);

/// AttributionStateManager manages the state of the published attribution claims as known by
/// [AttributionClient].
struct AttributionStateManager {
    /// Source to Principal definitions.
    attribution_providers: AttributionState,

    /// Counter for GlobalPrincipalIdentifiers.
    next_global_id: GlobalPrincipalIdentifierFactory,
}

impl AttributionStateManager {
    // |root_job_koid| is used to anchor the attribution hierarchy to the root of the Zircon
    // job hierarchy. This ensures no memory is left unaccounted.
    pub fn new_with_root_job(
        root_job_koid: u64,
    ) -> (AttributionStateManager, GlobalPrincipalIdentifier) {
        let mut attribution_providers = AttributionState::default();

        let mut principal_id_factory = GlobalPrincipalIdentifierFactory::new();

        let root_identifier = principal_id_factory.next();
        let principal = PrincipalDefinition {
            attributor: None,
            id: root_identifier,
            description: PrincipalDescription::Component("root".to_owned()),
            principal_type: PrincipalType::Runnable,
        };
        attribution_providers.0.insert(
            root_identifier,
            AttributionProvider {
                definitions: HashMap::from([(
                    LocalPrincipalIdentifier::self_identifier(),
                    principal,
                )]),
                resources: HashMap::from([(
                    LocalPrincipalIdentifier::self_identifier(),
                    vec![fattribution::Resource::KernelObject(root_job_koid)],
                )]),
            },
        );

        (
            AttributionStateManager { attribution_providers, next_global_id: principal_id_factory },
            root_identifier,
        )
    }

    /// Add a new Attribution Provider, from principal `identifier`, to the state.
    fn add_new_provider(&mut self, identifier: GlobalPrincipalIdentifier) {
        self.attribution_providers.0.insert(
            identifier,
            AttributionProvider { definitions: HashMap::new(), resources: HashMap::new() },
        );
    }

    /// Add a new principal, from `attributor`, to the state.
    fn add_new_principal(
        &mut self,
        attributor: GlobalPrincipalIdentifier,
        local: LocalPrincipalIdentifier,
        description: PrincipalDescription,
        principal_type: PrincipalType,
    ) -> GlobalPrincipalIdentifier {
        let global_id = self.next_global_id.next();
        self.attribution_providers.0.get_mut(&attributor).unwrap().definitions.insert(
            local,
            PrincipalDefinition {
                attributor: Some(attributor),
                id: global_id,
                description,
                principal_type,
            },
        );
        global_id
    }
}

/// AttributionClient is a client of a hierarchy of attribution providers, who provide the Provider
/// FIDL protocol. It resolves component moniker names using the Introspector protocol from
/// Component Manager.
pub struct AttributionClient {
    introspector: fcomponent::IntrospectorProxy,

    attribution_state: Mutex<AttributionStateManager>,
}

impl AttributionClient {
    /// Creates and starts the memory attribution client.
    pub fn new(
        root_provider: fattribution::ProviderProxy,
        introspector: fcomponent::IntrospectorProxy,
        root_job_koid: zx::Koid,
    ) -> Arc<AttributionClient> {
        let (manager, root_id) =
            AttributionStateManager::new_with_root_job(root_job_koid.raw_koid());
        let client = Arc::new(AttributionClient {
            introspector: introspector,
            attribution_state: Mutex::new(manager),
        });
        Self::attribute_memory(root_id, root_provider, client.clone());
        client
    }

    /// Starts a new asynchronous task to gather memory attribution information from the
    /// `source_principal` Principal exposing the `provider` interface, and stores this information
    /// into `client`.
    fn attribute_memory(
        source_principal: GlobalPrincipalIdentifier,
        provider: fattribution::ProviderProxy,
        client: Arc<AttributionClient>,
    ) {
        fuchsia_async::Task::spawn(Self::attribute_memory_logging_inner(
            source_principal,
            provider,
            client,
        ))
        .detach();
    }

    async fn attribute_memory_logging_inner(
        source_principal: GlobalPrincipalIdentifier,
        provider: fattribution::ProviderProxy,
        client: Arc<AttributionClient>,
    ) {
        let result = Self::attribute_memory_inner(source_principal, provider, client.clone()).await;
        if let Err(err) = result {
            error!("Error while attributing memory: {:?}", err)
        }
        client.attribution_state.lock().attribution_providers.0.remove(&source_principal);
    }

    async fn attribute_memory_inner(
        source_principal: GlobalPrincipalIdentifier,
        provider: fattribution::ProviderProxy,
        client: Arc<AttributionClient>,
    ) -> Result<(), AttributionClientError> {
        loop {
            let attributions = match provider.get().await {
                Ok(response) => response?,
                Err(err) => {
                    if let fidl::Error::ClientChannelClosed { status: _, protocol_name: _ } = err {
                        // The server disconnected voluntarily. This is the expected behavior when
                        // a Principal shuts down, so we just need to clean up without throwing an
                        // error.
                        return Ok(());
                    } else {
                        // This is a real error, so we need to report it.
                        return Err(err.into());
                    }
                }
            };

            // If there are children, resources assigned to this node by its parent
            // will be re-assigned to children if applicable.
            for attribution in attributions.attributions.into_iter().flatten() {
                // Recursively attribute memory in this child principal.
                match attribution {
                    fattribution::AttributionUpdate::Add(new_principal) => {
                        let local_identifier = LocalPrincipalIdentifier(
                            new_principal.identifier.ok_or_else(|| {
                                AttributionClientError::MissingField(
                                    "NewPrincipal::identifier".to_owned(),
                                )
                            })?,
                        );

                        let introspector = &client.introspector;
                        let description =
                            PrincipalDescription::new(new_principal.description, introspector)
                                .await?;
                        let principal_type = new_principal
                            .principal_type
                            .ok_or_else(|| {
                                AttributionClientError::MissingField(
                                    "NewPrincipal::type_".to_owned(),
                                )
                            })?
                            .try_into()?;
                        let child_id = client.attribution_state.lock().add_new_principal(
                            source_principal.clone(),
                            local_identifier,
                            description,
                            principal_type,
                        );

                        if let Some(child_provider) = new_principal.detailed_attribution {
                            client.attribution_state.lock().add_new_provider(child_id);
                            Self::attribute_memory(
                                child_id,
                                child_provider.into_proxy(),
                                client.clone(),
                            );
                        };
                    }
                    fattribution::AttributionUpdate::Update(updated_principal) => {
                        let identifier = LocalPrincipalIdentifier(
                            updated_principal.identifier.ok_or_else(|| {
                                AttributionClientError::MissingField(
                                    "UpdatedPrincipal::identifier".to_owned(),
                                )
                            })?,
                        );

                        let resources_opt =
                            updated_principal.resources.map(|resources| match resources {
                                fattribution::Resources::Data(d) => d.resources,
                                fattribution::Resources::Buffer(b) => {
                                    let mapping =
                                        mapped_vmo::ImmutableMapping::create_from_vmo(&b, false)
                                            .unwrap();
                                    let resource_vector: fattribution::Data =
                                        fidl::unpersist(&mapping).unwrap();
                                    resource_vector.resources
                                }
                                _ => todo!(),
                            });
                        match resources_opt {
                            None => {
                                client
                                    .attribution_state
                                    .lock()
                                    .attribution_providers
                                    .0
                                    .get_mut(&source_principal)
                                    .unwrap()
                                    .resources
                                    .remove(&identifier);
                            }
                            Some(resources) => {
                                client
                                    .attribution_state
                                    .lock()
                                    .attribution_providers
                                    .0
                                    .get_mut(&source_principal)
                                    .unwrap()
                                    .resources
                                    .insert(identifier.clone(), resources);
                            }
                        };
                    }
                    fattribution::AttributionUpdate::Remove(identifier) => {
                        debug_assert_ne!(identifier, 0);
                        let mut attribution_state = client.attribution_state.lock();
                        attribution_state
                            .attribution_providers
                            .0
                            .get_mut(&source_principal)
                            .unwrap()
                            .definitions
                            .remove(&LocalPrincipalIdentifier(identifier));
                        attribution_state
                            .attribution_providers
                            .0
                            .get_mut(&source_principal)
                            .unwrap()
                            .resources
                            .remove(&LocalPrincipalIdentifier(identifier));
                    }
                    _ => panic!("Unimplemented"),
                };
            }
        }
    }

    pub fn get_attributions(&self) -> AttributionState {
        self.attribution_state.lock().attribution_providers.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fidl::endpoints::RequestStream;
    use fuchsia_async as fasync;
    use futures::TryStreamExt;

    /// Tests a two-level attribution hierarchy.
    #[test]
    fn test_attribute_memory() {
        let mut exec = fasync::TestExecutor::new();
        let (root_provider, snapshot_request_stream) =
            fidl::endpoints::create_proxy_and_stream::<fattribution::ProviderMarker>().unwrap();
        let (introspector, mut introspector_request_stream) =
            fidl::endpoints::create_proxy_and_stream::<fcomponent::IntrospectorMarker>().unwrap();

        let root_job_koid = zx::Koid::from_raw(1);
        let attribution_client = AttributionClient::new(root_provider, introspector, root_job_koid);

        let server = attribution_server::AttributionServer::new(Box::new(|| {
            let new_principal = fattribution::NewPrincipal {
                identifier: Some(2),
                description: Some(fattribution::Description::Part("part".to_owned())),
                principal_type: Some(fattribution::PrincipalType::Runnable),
                detailed_attribution: None,
                ..Default::default()
            };
            vec![fattribution::AttributionUpdate::Add(new_principal)]
        }));

        let observer = server.new_observer(snapshot_request_stream.control_handle());
        fasync::Task::spawn(async move {
            serve(observer, snapshot_request_stream).await.unwrap();
        })
        .detach();

        fasync::Task::spawn(async move {
            while let Some(request) = introspector_request_stream.try_next().await.unwrap() {
                match request {
                    fcomponent::IntrospectorRequest::GetMoniker {
                        component_instance,
                        responder,
                    } => {
                        responder.send(Ok(&format!("{:?}", component_instance))).unwrap();
                    }
                    fcomponent::IntrospectorRequest::_UnknownMethod { ordinal, .. } => {
                        unimplemented!("Unknown method {}", ordinal)
                    }
                }
            }
        })
        .detach();

        let mut never_finishing_future = std::future::pending::<()>();
        let _ = exec.run_until_stalled(&mut never_finishing_future);

        let state = attribution_client.attribution_state.lock().attribution_providers.clone();
        assert_eq!(state.0.len(), 1);
        let (_, provider) = state.0.iter().next().unwrap();
        assert_eq!(
            provider.definitions.get(&LocalPrincipalIdentifier(2)),
            Some(&PrincipalDefinition {
                attributor: Some(1.into()),
                id: 2.into(),
                description: PrincipalDescription::Part("part".to_owned()),
                principal_type: PrincipalType::Runnable
            })
        );
    }

    pub async fn serve(
        observer: attribution_server::Observer,
        mut stream: fattribution::ProviderRequestStream,
    ) -> Result<(), fidl::Error> {
        while let Some(request) = stream.try_next().await? {
            match request {
                fattribution::ProviderRequest::Get { responder } => {
                    observer.next(responder);
                }
                fattribution::ProviderRequest::_UnknownMethod { .. } => {
                    assert!(false);
                }
            }
        }
        Ok(())
    }
}
