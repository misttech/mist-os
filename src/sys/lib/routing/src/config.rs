// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::policy::allowlist_entry_matches,
    anyhow::{format_err, Context, Error},
    cm_rust::{CapabilityTypeName, FidlIntoNative},
    cm_types::{symmetrical_enums, Name, ParseError, Url},
    fidl::unpersist,
    fidl_fuchsia_component_decl as fdecl,
    fidl_fuchsia_component_internal::{
        self as component_internal, BuiltinBootResolver, CapabilityPolicyAllowlists,
        DebugRegistrationPolicyAllowlists, LogDestination, RealmBuilderResolverAndRunner,
    },
    moniker::{
        AbsoluteMoniker, AbsoluteMonikerBase, ChildMoniker, ChildMonikerBase, ExtendedMoniker,
        MonikerError,
    },
    std::{
        collections::{HashMap, HashSet},
        convert::TryFrom,
        iter::FromIterator,
        path::Path,
    },
    thiserror::Error,
    tracing::warn,
    version_history::AbiRevision,
};

/// Runtime configuration options.
/// This configuration intended to be "global", in that the same configuration
/// is applied throughout a given running instance of component_manager.
#[derive(Debug, PartialEq, Eq)]
pub struct RuntimeConfig {
    /// How many children, maximum, are returned by a call to `ChildIterator.next()`.
    pub list_children_batch_size: usize,

    /// Security policy configuration.
    pub security_policy: SecurityPolicy,

    /// If true, component manager will be in debug mode. In this mode, component manager
    /// provides the `EventSource` protocol and exposes this protocol. The root component
    /// must be manually started using the LifecycleController protocol in the hub.
    ///
    /// This is done so that an external component (say an integration test) can subscribe
    /// to events before the root component has started.
    pub debug: bool,

    /// Enables Component Manager's introspection APIs (RealmQuery, RealmExplorer,
    /// RouteValidator, LifecycleController, etc.) for use by components.
    pub enable_introspection: bool,

    /// If true, component_manager will serve an instance of fuchsia.process.Launcher and use this
    /// launcher for the built-in ELF component runner. The root component can additionally
    /// use and/or offer this service using '/builtin/fuchsia.process.Launcher' from realm.
    // This flag exists because the built-in process launcher *only* works when
    // component_manager runs under a job that has ZX_POL_NEW_PROCESS set to allow, like the root
    // job. Otherwise, the component_manager process cannot directly create process through
    // zx_process_create. When we run component_manager elsewhere, like in test environments, it
    // has to use the fuchsia.process.Launcher service provided through its namespace instead.
    pub use_builtin_process_launcher: bool,

    /// If true, component_manager will maintain a UTC kernel clock and vend write handles through
    /// an instance of `fuchsia.time.Maintenance`. This flag should only be used with the top-level
    /// component_manager.
    pub maintain_utc_clock: bool,

    // The number of threads to use for running component_manager's executor.
    // Value defaults to 1.
    pub num_threads: usize,

    /// The list of capabilities offered from component manager's namespace.
    pub namespace_capabilities: Vec<cm_rust::CapabilityDecl>,

    /// The list of capabilities offered from component manager as built-in capabilities.
    pub builtin_capabilities: Vec<cm_rust::CapabilityDecl>,

    /// URL of the root component to launch. This field is used if no URL
    /// is passed to component manager. If value is passed in both places, then
    /// an error is raised.
    pub root_component_url: Option<Url>,

    /// Path to the component ID index, parsed from
    /// `fuchsia.component.internal.RuntimeConfig.component_id_index_path`.
    pub component_id_index_path: Option<String>,

    /// Where to log to.
    pub log_destination: LogDestination,

    /// If true, component manager will log all events dispatched in the topology.
    pub log_all_events: bool,

    /// Which builtin resolver to use for the fuchsia-boot scheme. If not supplied this defaults to
    /// the NONE option.
    pub builtin_boot_resolver: BuiltinBootResolver,

    /// If true, allow components to set the `OnTerminate=REBOOT` option.
    ///
    /// This lets a parent component designate that the system should reboot if a child terminates
    /// (except when it's shut down).
    pub reboot_on_terminate_enabled: bool,

    /// If and how the realm builder resolver and runner are enabled.
    pub realm_builder_resolver_and_runner: RealmBuilderResolverAndRunner,

    /// The enforcement and validation policy to apply to component target ABI revisions.
    pub abi_revision_policy: AbiRevisionPolicy,
}

/// A single security policy allowlist entry.
#[derive(Debug, PartialEq, Eq, Hash, Clone)]
pub struct AllowlistEntry {
    // A list of matchers that apply to each child in a moniker.
    // If this list is empty, we must only allow the root moniker.
    pub matchers: Vec<AllowlistMatcher>,
}

#[derive(Debug, PartialEq, Eq, Hash, Clone)]
pub enum AllowlistMatcher {
    /// Allow the child with this exact ChildMoniker.
    /// Examples: "bar", "foo:bar", "baz"
    Exact(ChildMoniker),
    /// Allow any descendant of this realm.
    /// This is indicated by "**" in a config file.
    AnyDescendant,
    /// Allow any child of this realm.
    /// This is indicated by "*" in a config file.
    AnyChild,
    /// Allow any child of a particular collection in this realm.
    /// This is indicated by "<collection>:*" in a config file.
    AnyChildInCollection(Name),
    /// Allow any descendant of a particular collection in this realm.
    /// This is indicated by "<collection>:**" in a config file.
    AnyDescendantInCollection(Name),
}

pub struct AllowlistEntryBuilder {
    parts: Vec<AllowlistMatcher>,
}

impl AllowlistEntryBuilder {
    pub fn new() -> Self {
        Self { parts: vec![] }
    }

    pub fn build_exact_from_moniker(m: &AbsoluteMoniker) -> AllowlistEntry {
        Self::new().exact_from_moniker(m).build()
    }

    pub fn exact(mut self, name: &str) -> Self {
        self.parts.push(AllowlistMatcher::Exact(ChildMoniker::parse(name).unwrap()));
        self
    }

    pub fn exact_from_moniker(mut self, m: &AbsoluteMoniker) -> Self {
        let mut parts = m.path().clone().into_iter().map(|c| AllowlistMatcher::Exact(c)).collect();
        self.parts.append(&mut parts);
        self
    }

    pub fn any_child(mut self) -> Self {
        self.parts.push(AllowlistMatcher::AnyChild);
        self
    }

    pub fn any_descendant(mut self) -> AllowlistEntry {
        self.parts.push(AllowlistMatcher::AnyDescendant);
        self.build()
    }

    pub fn any_descendant_in_collection(mut self, collection: &str) -> AllowlistEntry {
        self.parts
            .push(AllowlistMatcher::AnyDescendantInCollection(Name::new(collection).unwrap()));
        self.build()
    }

    pub fn any_child_in_collection(mut self, collection: &str) -> Self {
        self.parts.push(AllowlistMatcher::AnyChildInCollection(Name::new(collection).unwrap()));
        self
    }

    pub fn build(self) -> AllowlistEntry {
        AllowlistEntry { matchers: self.parts }
    }
}

/// Runtime security policy.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SecurityPolicy {
    /// Allowlists for Zircon job policy.
    pub job_policy: JobPolicyAllowlists,

    /// Capability routing policies. The key contains all the information required
    /// to uniquely identify any routable capability and the set of monikers
    /// define the set of component paths that are allowed to access this specific
    /// capability.
    pub capability_policy: HashMap<CapabilityAllowlistKey, HashSet<AllowlistEntry>>,

    /// Debug Capability routing policies. The key contains all the absolute information
    /// needed to identify a routable capability and the set of DebugCapabilityAllowlistEntries
    /// define the allowed set of routing paths from the capability source to the environment
    /// offering the capability.
    pub debug_capability_policy:
        HashMap<DebugCapabilityKey, HashSet<DebugCapabilityAllowlistEntry>>,

    /// Allowlists component child policy. These allowlists control what components are allowed
    /// to set privileged options on their children.
    pub child_policy: ChildPolicyAllowlists,
}

/// Allowlist key for debug capability allowlists.
/// This defines all portions of the allowlist that do not support globbing.
#[derive(Debug, Hash, PartialEq, Eq, Clone)]
pub struct DebugCapabilityKey {
    pub source_name: Name,
    pub source: CapabilityAllowlistSource,
    pub capability: CapabilityTypeName,
    pub env_name: String,
}

/// Represents a single allowed route for a debug capability.
#[derive(Debug, PartialEq, Eq, Clone, Hash)]
pub struct DebugCapabilityAllowlistEntry {
    source: AllowlistEntry,
    dest: AllowlistEntry,
}

impl DebugCapabilityAllowlistEntry {
    pub fn new(source: AllowlistEntry, dest: AllowlistEntry) -> Self {
        Self { source, dest }
    }

    pub fn matches(&self, source: &ExtendedMoniker, dest: &AbsoluteMoniker) -> bool {
        let source_absolute = match source {
            // Currently no debug capabilities are routed from cm.
            ExtendedMoniker::ComponentManager => return false,
            ExtendedMoniker::ComponentInstance(ref moniker) => moniker,
        };
        allowlist_entry_matches(&self.source, source_absolute)
            && allowlist_entry_matches(&self.dest, dest)
    }
}

/// Allowlists for Zircon job policy. Part of runtime security policy.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct JobPolicyAllowlists {
    /// Entries for components allowed to be given the ZX_POL_AMBIENT_MARK_VMO_EXEC job policy.
    ///
    /// Components must request this policy by including "job_policy_ambient_mark_vmo_exec: true" in
    /// their manifest's program object and must be using the ELF runner.
    /// This is equivalent to the v1 'deprecated-ambient-replace-as-executable' feature.
    pub ambient_mark_vmo_exec: Vec<AllowlistEntry>,

    /// Entries for components allowed to have their original process marked as critical to
    /// component_manager's job.
    ///
    /// Components must request this critical marking by including "main_process_critical: true" in
    /// their manifest's program object and must be using the ELF runner.
    pub main_process_critical: Vec<AllowlistEntry>,

    /// Entries for components allowed to call zx_process_create directly (e.g., do not have
    /// ZX_POL_NEW_PROCESS set to ZX_POL_ACTION_DENY).
    ///
    /// Components must request this policy by including "job_policy_create_raw_processes: true" in
    /// their manifest's program object and must be using the ELF runner.
    pub create_raw_processes: Vec<AllowlistEntry>,
}

/// Allowlists for child option policy. Part of runtime security policy.
#[derive(Debug, Default, PartialEq, Eq, Clone)]
pub struct ChildPolicyAllowlists {
    /// Absolute monikers of component instances allowed to have the
    /// `on_terminate=REBOOT` in their `children` declaration.
    pub reboot_on_terminate: Vec<AllowlistEntry>,
}

/// The available capability sources for capability allow lists. This is a strict
/// subset of all possible Ref types, with equality support.
#[derive(Debug, PartialEq, Eq, Hash, Clone)]
pub enum CapabilityAllowlistSource {
    Self_,
    Framework,
    Capability,
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum AbiRevisionError {
    #[error("Missing a component target ABI revision.")]
    Absent,
    #[error("Unsupported component target ABI revision: {0}. The following revisions are supported: {1}")]
    Unsupported(AbiRevision, String),
}

/// The enforcement and validation policy to apply to component target ABI revisions.
/// Defaults to `AllowAll`
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum AbiRevisionPolicy {
    AllowAll,
    EnforcePresenceOnly,
    EnforcePresenceAndCompatibility,
}

symmetrical_enums!(
    AbiRevisionPolicy,
    component_internal::AbiRevisionPolicy,
    AllowAll,
    EnforcePresenceOnly,
    EnforcePresenceAndCompatibility
);

impl Default for AbiRevisionPolicy {
    fn default() -> Self {
        AbiRevisionPolicy::AllowAll
    }
}

impl AbiRevisionPolicy {
    fn is_supported(moniker: &AbsoluteMoniker, abi_revision: Option<AbiRevision>) -> bool {
        match abi_revision {
            Some(abi) => {
                let is_supported = version_history::is_supported_abi_revision(abi);
                if !is_supported {
                    warn!("Component {} targets an invalid ABI revision {}.", moniker, abi)
                }
                is_supported
            }
            None => {
                warn!("Component {} does not have a target ABI revision.", moniker);
                false
            }
        }
    }
    /// Check if the abi_revision, if present, is supported by the platform and compatible with the
    /// `AbiRevisionPolicy`. Regardless of the enforcement policy, log a warning if the
    /// ABI revision is missing or not supported by the platform.
    pub fn check_compatibility(
        &self,
        moniker: &AbsoluteMoniker,
        abi_revision: Option<AbiRevision>,
    ) -> Result<(), AbiRevisionError> {
        let is_supported_abi = Self::is_supported(moniker, abi_revision);
        match (self, abi_revision) {
            (AbiRevisionPolicy::AllowAll, _) => Ok(()),
            (AbiRevisionPolicy::EnforcePresenceOnly, Some(_)) => Ok(()),
            (AbiRevisionPolicy::EnforcePresenceAndCompatibility, Some(abi)) => {
                if is_supported_abi {
                    Ok(())
                } else {
                    let supported_abis: Vec<_> = version_history::get_supported_abi_revisions()
                        .into_iter()
                        .map(|v| format!("{}", version_history::AbiRevision(v)))
                        .collect();
                    Err(AbiRevisionError::Unsupported(abi, supported_abis.join(",")))
                }
            }
            _ => Err(AbiRevisionError::Absent),
        }
    }
}

/// Allowlist key for capability routing policy. Part of the runtime
/// security policy. This defines all the required keying information to lookup
/// whether a capability exists in the policy map or not.
#[derive(Debug, PartialEq, Eq, Hash, Clone)]
pub struct CapabilityAllowlistKey {
    pub source_moniker: ExtendedMoniker,
    pub source_name: Name,
    pub source: CapabilityAllowlistSource,
    pub capability: CapabilityTypeName,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            list_children_batch_size: 1000,
            // security_policy must default to empty to ensure that it fails closed if no
            // configuration is present or it fails to load.
            security_policy: Default::default(),
            debug: false,
            enable_introspection: false,
            use_builtin_process_launcher: false,
            maintain_utc_clock: false,
            num_threads: 1,
            namespace_capabilities: vec![],
            builtin_capabilities: vec![],
            root_component_url: Default::default(),
            component_id_index_path: None,
            log_destination: LogDestination::Syslog,
            log_all_events: false,
            builtin_boot_resolver: BuiltinBootResolver::None,
            reboot_on_terminate_enabled: false,
            realm_builder_resolver_and_runner: RealmBuilderResolverAndRunner::None,
            abi_revision_policy: Default::default(),
        }
    }
}

impl RuntimeConfig {
    /// Load RuntimeConfig from the '--config' command line arg. Path must
    /// point to binary encoded fuchsia.component.internal.Config file.
    /// Otherwise, an Error is returned.
    pub fn load_from_file<P: AsRef<Path>>(path: P) -> Result<Self, Error> {
        let raw_content = std::fs::read(path)?;
        Ok(Self::try_from(unpersist::<component_internal::Config>(&raw_content)?)?)
    }

    pub fn load_from_bytes(bytes: &Vec<u8>) -> Result<Self, Error> {
        Ok(Self::try_from(unpersist::<component_internal::Config>(&bytes)?)?)
    }

    fn translate_namespace_capabilities(
        capabilities: Option<Vec<fdecl::Capability>>,
    ) -> Result<Vec<cm_rust::CapabilityDecl>, Error> {
        let capabilities = capabilities.unwrap_or(vec![]);
        if let Some(c) = capabilities.iter().find(|c| {
            !matches!(c, fdecl::Capability::Protocol(_) | fdecl::Capability::Directory(_))
        }) {
            return Err(format_err!("Type unsupported for namespace capability: {:?}", c));
        }
        cm_fidl_validator::validate_capabilities(&capabilities, false)?;
        Ok(capabilities.into_iter().map(FidlIntoNative::fidl_into_native).collect())
    }

    fn translate_builtin_capabilities(
        capabilities: Option<Vec<fdecl::Capability>>,
    ) -> Result<Vec<cm_rust::CapabilityDecl>, Error> {
        let capabilities = capabilities.unwrap_or(vec![]);
        cm_fidl_validator::validate_capabilities(&capabilities, true)?;
        Ok(capabilities.into_iter().map(FidlIntoNative::fidl_into_native).collect())
    }
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum AllowlistEntryParseError {
    #[error("Invalid child moniker ({0:?}) in allowlist entry: {1:?}")]
    InvalidChildMoniker(String, #[source] MonikerError),
    #[error("Invalid collection name ({0:?}) in allowlist entry: {1:?}")]
    InvalidCollectionName(String, #[source] ParseError),
    #[error("Allowlist entry ({0:?}) must start with a '/'")]
    NoLeadingSlash(String),
    #[error("Allowlist entry ({0:?}) must have '**' wildcard only at the end")]
    DescendantWildcardOnlyAtEnd(String),
}

fn parse_allowlist_entries(strs: &Option<Vec<String>>) -> Result<Vec<AllowlistEntry>, Error> {
    let strs = match strs {
        Some(strs) => strs,
        None => return Ok(Vec::new()),
    };

    let mut entries = vec![];
    for input in strs {
        let entry = parse_allowlist_entry(input)?;
        entries.push(entry);
    }
    Ok(entries)
}

fn parse_allowlist_entry(input: &str) -> Result<AllowlistEntry, AllowlistEntryParseError> {
    let entry = if let Some(entry) = input.strip_prefix('/') {
        entry
    } else {
        return Err(AllowlistEntryParseError::NoLeadingSlash(input.to_string()));
    };

    if entry.is_empty() {
        return Ok(AllowlistEntry { matchers: vec![] });
    }

    if entry.contains("**") && !entry.ends_with("**") {
        return Err(AllowlistEntryParseError::DescendantWildcardOnlyAtEnd(input.to_string()));
    }

    let mut parts = vec![];
    for name in entry.split('/') {
        let part = match name {
            "**" => AllowlistMatcher::AnyDescendant,
            "*" => AllowlistMatcher::AnyChild,
            name => {
                if let Some(collection_name) = name.strip_suffix(":**") {
                    let collection_name = Name::new(collection_name).map_err(|e| {
                        AllowlistEntryParseError::InvalidCollectionName(
                            collection_name.to_string(),
                            e,
                        )
                    })?;
                    AllowlistMatcher::AnyDescendantInCollection(collection_name)
                } else if let Some(collection_name) = name.strip_suffix(":*") {
                    let collection_name = Name::new(collection_name).map_err(|e| {
                        AllowlistEntryParseError::InvalidCollectionName(
                            collection_name.to_string(),
                            e,
                        )
                    })?;
                    AllowlistMatcher::AnyChildInCollection(collection_name)
                } else {
                    let child_moniker = ChildMoniker::parse(name).map_err(|e| {
                        AllowlistEntryParseError::InvalidChildMoniker(name.to_string(), e)
                    })?;
                    AllowlistMatcher::Exact(child_moniker)
                }
            }
        };
        parts.push(part);
    }

    Ok(AllowlistEntry { matchers: parts })
}

fn as_usize_or_default(value: Option<u32>, default: usize) -> usize {
    match value {
        Some(value) => value as usize,
        None => default,
    }
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum PolicyConfigError {
    #[error("Capability source name was empty in a capability policy entry.")]
    EmptyCapabilitySourceName,
    #[error("Capability type was empty in a capability policy entry.")]
    EmptyAllowlistedCapability,
    #[error("Debug registration type was empty in a debug policy entry.")]
    EmptyAllowlistedDebugRegistration,
    #[error("Environment name was empty in a debug policy entry.")]
    EmptyTargetMonikerDebugRegistration,
    #[error("Target moniker was empty in a debug policy entry.")]
    EmptyEnvironmentNameDebugRegistration,
    #[error("Capability from type was empty in a capability policy entry.")]
    EmptyFromType,
    #[error("Capability source_moniker was empty in a capability policy entry.")]
    EmptySourceMoniker,
    #[error("Invalid source capability.")]
    InvalidSourceCapability,
    #[error("Unsupported allowlist capability type")]
    UnsupportedAllowlistedCapability,
}

impl TryFrom<component_internal::Config> for RuntimeConfig {
    type Error = Error;

    fn try_from(config: component_internal::Config) -> Result<Self, Error> {
        let default = RuntimeConfig::default();

        let list_children_batch_size =
            as_usize_or_default(config.list_children_batch_size, default.list_children_batch_size);
        let num_threads = as_usize_or_default(config.num_threads, default.num_threads);

        let root_component_url = match config.root_component_url {
            Some(url) => Some(Url::new(url)?),
            None => None,
        };

        let security_policy = if let Some(security_policy) = config.security_policy {
            SecurityPolicy::try_from(security_policy).context("Unable to parse security policy")?
        } else {
            SecurityPolicy::default()
        };

        let log_all_events =
            if let Some(log_all_events) = config.log_all_events { log_all_events } else { false };

        let abi_revision_policy =
            config.abi_revision_policy.map(AbiRevisionPolicy::from).unwrap_or_default();

        Ok(RuntimeConfig {
            list_children_batch_size,
            security_policy,
            namespace_capabilities: Self::translate_namespace_capabilities(
                config.namespace_capabilities,
            )?,
            builtin_capabilities: Self::translate_builtin_capabilities(
                config.builtin_capabilities,
            )?,
            debug: config.debug.unwrap_or(default.debug),
            enable_introspection: config
                .enable_introspection
                .unwrap_or(default.enable_introspection),
            use_builtin_process_launcher: config
                .use_builtin_process_launcher
                .unwrap_or(default.use_builtin_process_launcher),
            maintain_utc_clock: config.maintain_utc_clock.unwrap_or(default.maintain_utc_clock),
            num_threads,
            root_component_url,
            component_id_index_path: config.component_id_index_path,
            log_destination: config.log_destination.unwrap_or(default.log_destination),
            log_all_events,
            builtin_boot_resolver: config
                .builtin_boot_resolver
                .unwrap_or(default.builtin_boot_resolver),
            reboot_on_terminate_enabled: config
                .reboot_on_terminate_enabled
                .unwrap_or(default.reboot_on_terminate_enabled),
            realm_builder_resolver_and_runner: config
                .realm_builder_resolver_and_runner
                .unwrap_or(default.realm_builder_resolver_and_runner),
            abi_revision_policy,
        })
    }
}

fn parse_capability_policy(
    capability_policy: Option<CapabilityPolicyAllowlists>,
) -> Result<HashMap<CapabilityAllowlistKey, HashSet<AllowlistEntry>>, Error> {
    let capability_policy = if let Some(capability_policy) = capability_policy {
        if let Some(allowlist) = capability_policy.allowlist {
            let mut policies = HashMap::new();
            for e in allowlist.into_iter() {
                let source_moniker = ExtendedMoniker::parse_str(
                    e.source_moniker
                        .as_deref()
                        .ok_or(Error::new(PolicyConfigError::EmptySourceMoniker))?,
                )?;
                let source_name = if let Some(source_name) = e.source_name {
                    Ok(source_name
                        .parse()
                        .map_err(|_| Error::new(PolicyConfigError::InvalidSourceCapability))?)
                } else {
                    Err(PolicyConfigError::EmptyCapabilitySourceName)
                }?;
                let source = match e.source {
                    Some(fdecl::Ref::Self_(_)) => Ok(CapabilityAllowlistSource::Self_),
                    Some(fdecl::Ref::Framework(_)) => Ok(CapabilityAllowlistSource::Framework),
                    Some(fdecl::Ref::Capability(_)) => Ok(CapabilityAllowlistSource::Capability),
                    _ => Err(Error::new(PolicyConfigError::InvalidSourceCapability)),
                }?;

                let capability = if let Some(capability) = e.capability.as_ref() {
                    match &capability {
                        component_internal::AllowlistedCapability::Directory(_) => {
                            Ok(CapabilityTypeName::Directory)
                        }
                        component_internal::AllowlistedCapability::Protocol(_) => {
                            Ok(CapabilityTypeName::Protocol)
                        }
                        component_internal::AllowlistedCapability::Service(_) => {
                            Ok(CapabilityTypeName::Service)
                        }
                        component_internal::AllowlistedCapability::Storage(_) => {
                            Ok(CapabilityTypeName::Storage)
                        }
                        component_internal::AllowlistedCapability::Runner(_) => {
                            Ok(CapabilityTypeName::Runner)
                        }
                        component_internal::AllowlistedCapability::Resolver(_) => {
                            Ok(CapabilityTypeName::Resolver)
                        }
                        _ => Err(Error::new(PolicyConfigError::EmptyAllowlistedCapability)),
                    }
                } else {
                    Err(Error::new(PolicyConfigError::EmptyAllowlistedCapability))
                }?;

                let target_monikers =
                    HashSet::from_iter(parse_allowlist_entries(&e.target_monikers)?);

                policies.insert(
                    CapabilityAllowlistKey { source_moniker, source_name, source, capability },
                    target_monikers,
                );
            }
            policies
        } else {
            HashMap::new()
        }
    } else {
        HashMap::new()
    };
    Ok(capability_policy)
}

fn parse_debug_capability_policy(
    debug_registration_policy: Option<DebugRegistrationPolicyAllowlists>,
) -> Result<HashMap<DebugCapabilityKey, HashSet<DebugCapabilityAllowlistEntry>>, Error> {
    let debug_capability_policy = if let Some(debug_capability_policy) = debug_registration_policy {
        if let Some(allowlist) = debug_capability_policy.allowlist {
            let mut policies: HashMap<DebugCapabilityKey, HashSet<DebugCapabilityAllowlistEntry>> =
                HashMap::new();
            for e in allowlist.into_iter() {
                let source_moniker = parse_allowlist_entry(
                    e.source_moniker
                        .as_deref()
                        .ok_or(Error::new(PolicyConfigError::EmptySourceMoniker))?,
                )?;
                let source_name = if let Some(source_name) = e.source_name.as_ref() {
                    Ok(source_name
                        .parse()
                        .map_err(|_| Error::new(PolicyConfigError::InvalidSourceCapability))?)
                } else {
                    Err(PolicyConfigError::EmptyCapabilitySourceName)
                }?;

                let capability = if let Some(capability) = e.debug.as_ref() {
                    match &capability {
                        component_internal::AllowlistedDebugRegistration::Protocol(_) => {
                            Ok(CapabilityTypeName::Protocol)
                        }
                        _ => Err(Error::new(PolicyConfigError::EmptyAllowlistedDebugRegistration)),
                    }
                } else {
                    Err(Error::new(PolicyConfigError::EmptyAllowlistedDebugRegistration))
                }?;

                let target_moniker = parse_allowlist_entry(
                    e.target_moniker
                        .as_deref()
                        .ok_or(PolicyConfigError::EmptyTargetMonikerDebugRegistration)?,
                )?;
                let env_name = e
                    .environment_name
                    .ok_or(PolicyConfigError::EmptyEnvironmentNameDebugRegistration)?;

                let key = DebugCapabilityKey {
                    source_name,
                    source: CapabilityAllowlistSource::Self_,
                    capability,
                    env_name,
                };
                let value = DebugCapabilityAllowlistEntry::new(source_moniker, target_moniker);
                if let Some(h) = policies.get_mut(&key) {
                    h.insert(value);
                } else {
                    policies.insert(key, vec![value].into_iter().collect());
                }
            }
            policies
        } else {
            HashMap::new()
        }
    } else {
        HashMap::new()
    };
    Ok(debug_capability_policy)
}

impl TryFrom<component_internal::SecurityPolicy> for SecurityPolicy {
    type Error = Error;

    fn try_from(security_policy: component_internal::SecurityPolicy) -> Result<Self, Error> {
        let job_policy = if let Some(job_policy) = &security_policy.job_policy {
            let ambient_mark_vmo_exec = parse_allowlist_entries(&job_policy.ambient_mark_vmo_exec)?;
            let main_process_critical = parse_allowlist_entries(&job_policy.main_process_critical)?;
            let create_raw_processes = parse_allowlist_entries(&job_policy.create_raw_processes)?;
            JobPolicyAllowlists {
                ambient_mark_vmo_exec,
                main_process_critical,
                create_raw_processes,
            }
        } else {
            JobPolicyAllowlists::default()
        };

        let capability_policy = parse_capability_policy(security_policy.capability_policy)?;

        let debug_capability_policy =
            parse_debug_capability_policy(security_policy.debug_registration_policy)?;

        let child_policy = if let Some(child_policy) = &security_policy.child_policy {
            let reboot_on_terminate = parse_allowlist_entries(&child_policy.reboot_on_terminate)?;
            ChildPolicyAllowlists { reboot_on_terminate }
        } else {
            ChildPolicyAllowlists::default()
        };

        Ok(SecurityPolicy { job_policy, capability_policy, debug_capability_policy, child_policy })
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*, assert_matches::assert_matches, cm_types::ParseError, fidl_fuchsia_io as fio,
        std::path::PathBuf, tempfile::TempDir,
    };

    const FOO_PKG_URL: &str = "fuchsia-pkg://fuchsia.com/foo#meta/foo.cmx";

    macro_rules! test_function_ok {
        ( $function:path, $($test_name:ident => ($input:expr, $expected:expr)),+ ) => {
            $(
                #[test]
                fn $test_name() {
                    assert_matches!($function($input), Ok(v) if v == $expected);
                }
            )+
        };
    }

    macro_rules! test_function_err {
        ( $function:path, $($test_name:ident => ($input:expr, $type:ty, $expected:expr)),+ ) => {
            $(
                #[test]
                fn $test_name() {
                    assert_eq!(*$function($input).unwrap_err().downcast_ref::<$type>().unwrap(), $expected);
                }
            )+
        };
    }

    macro_rules! test_config_ok {
        ( $($test_name:ident => ($input:expr, $expected:expr)),+ $(,)? ) => {
            test_function_ok! { RuntimeConfig::try_from, $($test_name => ($input, $expected)),+ }
        };
    }

    macro_rules! test_config_err {
        ( $($test_name:ident => ($input:expr, $type:ty, $expected:expr)),+ $(,)? ) => {
            test_function_err! { RuntimeConfig::try_from, $($test_name => ($input, $type, $expected)),+ }
        };
    }

    test_config_ok! {
        all_fields_none => (component_internal::Config {
            debug: None,
            enable_introspection: None,
            list_children_batch_size: None,
            security_policy: None,
            maintain_utc_clock: None,
            use_builtin_process_launcher: None,
            num_threads: None,
            namespace_capabilities: None,
            builtin_capabilities: None,
            root_component_url: None,
            component_id_index_path: None,
            reboot_on_terminate_enabled: None,
            ..Default::default()
        }, RuntimeConfig::default()),
        all_leaf_nodes_none => (component_internal::Config {
            debug: Some(false),
            enable_introspection: Some(false),
            list_children_batch_size: Some(5),
            maintain_utc_clock: Some(false),
            use_builtin_process_launcher: Some(true),
            security_policy: Some(component_internal::SecurityPolicy {
                job_policy: Some(component_internal::JobPolicyAllowlists {
                    main_process_critical: None,
                    ambient_mark_vmo_exec: None,
                    create_raw_processes: None,
                    ..Default::default()
                }),
                capability_policy: None,
                ..Default::default()
            }),
            num_threads: Some(10),
            namespace_capabilities: None,
            builtin_capabilities: None,
            root_component_url: None,
            component_id_index_path: None,
            log_destination: None,
            log_all_events: None,
            reboot_on_terminate_enabled: None,
            ..Default::default()
        }, RuntimeConfig {
            debug: false,
            enable_introspection: false,
            list_children_batch_size: 5,
            maintain_utc_clock: false,
            use_builtin_process_launcher:true,
            num_threads: 10,
            ..Default::default() }),
        all_fields_some => (
            component_internal::Config {
                debug: Some(true),
                enable_introspection: Some(true),
                list_children_batch_size: Some(42),
                maintain_utc_clock: Some(true),
                use_builtin_process_launcher: Some(false),
                security_policy: Some(component_internal::SecurityPolicy {
                    job_policy: Some(component_internal::JobPolicyAllowlists {
                        main_process_critical: Some(vec!["/something/important".to_string()]),
                        ambient_mark_vmo_exec: Some(vec!["/".to_string(), "/foo/bar".to_string()]),
                        create_raw_processes: Some(vec!["/another/thing".to_string()]),
                        ..Default::default()
                    }),
                    capability_policy: Some(component_internal::CapabilityPolicyAllowlists {
                        allowlist: Some(vec![
                        component_internal::CapabilityAllowlistEntry {
                            source_moniker: Some("<component_manager>".to_string()),
                            source_name: Some("fuchsia.boot.RootResource".to_string()),
                            source: Some(fdecl::Ref::Self_(fdecl::SelfRef {})),
                            capability: Some(component_internal::AllowlistedCapability::Protocol(component_internal::AllowlistedProtocol::default())),
                            target_monikers: Some(vec![
                                "/bootstrap".to_string(),
                                "/core/**".to_string(),
                                "/core/test_manager/tests:**".to_string()
                            ]),
                            ..Default::default()
                        },
                    ]), ..Default::default()}),
                    debug_registration_policy: Some(component_internal::DebugRegistrationPolicyAllowlists{
                        allowlist: Some(vec![
                            component_internal::DebugRegistrationAllowlistEntry {
                                source_moniker: Some("/foo/bar/baz".to_string()),
                                source_name: Some("fuchsia.foo.bar".to_string()),
                                debug: Some(component_internal::AllowlistedDebugRegistration::Protocol(component_internal::AllowlistedProtocol::default())),
                                target_moniker: Some("/foo/bar".to_string()),
                                environment_name: Some("bar_env1".to_string()),
                                ..Default::default()
                            },
                            component_internal::DebugRegistrationAllowlistEntry {
                                source_moniker: Some("/foo/bar/baz".to_string()),
                                source_name: Some("fuchsia.foo.bar".to_string()),
                                debug: Some(component_internal::AllowlistedDebugRegistration::Protocol(component_internal::AllowlistedProtocol::default())),
                                target_moniker: Some("/foo".to_string()),
                                environment_name: Some("foo_env1".to_string()),
                                ..Default::default()
                            },
                            component_internal::DebugRegistrationAllowlistEntry {
                                source_moniker: Some("/foo/bar/**".to_string()),
                                source_name: Some("fuchsia.foo.baz".to_string()),
                                debug: Some(component_internal::AllowlistedDebugRegistration::Protocol(component_internal::AllowlistedProtocol::default())),
                                target_moniker: Some("/foo/**".to_string()),
                                environment_name: Some("foo_env2".to_string()),
                                ..Default::default()
                            },
                            component_internal::DebugRegistrationAllowlistEntry {
                                source_moniker: Some("/foo/bar/coll:**".to_string()),
                                source_name: Some("fuchsia.foo.baz".to_string()),
                                debug: Some(component_internal::AllowlistedDebugRegistration::Protocol(component_internal::AllowlistedProtocol::default())),
                                target_moniker: Some("/root".to_string()),
                                environment_name: Some("root_env".to_string()),
                                ..Default::default()
                            },
                        ]), ..Default::default()}),
                    child_policy: Some(component_internal::ChildPolicyAllowlists {
                        reboot_on_terminate: Some(vec!["/something/important".to_string()]),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                num_threads: Some(24),
                namespace_capabilities: Some(vec![
                    fdecl::Capability::Protocol(fdecl::Protocol {
                        name: Some("foo_svc".into()),
                        source_path: Some("/svc/foo".into()),
                        ..Default::default()
                    }),
                    fdecl::Capability::Directory(fdecl::Directory {
                        name: Some("bar_dir".into()),
                        source_path: Some("/bar".into()),
                        rights: Some(fio::Operations::CONNECT),
                        ..Default::default()
                    }),
                ]),
                builtin_capabilities: Some(vec![
                    fdecl::Capability::Protocol(fdecl::Protocol {
                        name: Some("foo_protocol".into()),
                        source_path: None,
                        ..Default::default()
                    }),
                ]),
                root_component_url: Some(FOO_PKG_URL.to_string()),
                component_id_index_path: Some("/boot/config/component_id_index".to_string()),
                log_destination: Some(component_internal::LogDestination::Klog),
                log_all_events: Some(true),
                builtin_boot_resolver: Some(component_internal::BuiltinBootResolver::None),
                reboot_on_terminate_enabled: Some(true),
                realm_builder_resolver_and_runner: Some(component_internal::RealmBuilderResolverAndRunner::None),
                abi_revision_policy: Some(component_internal::AbiRevisionPolicy::AllowAll),
                ..Default::default()
            },
            RuntimeConfig {
                abi_revision_policy: AbiRevisionPolicy::AllowAll,
                debug: true,
                enable_introspection: true,
                list_children_batch_size: 42,
                maintain_utc_clock: true,
                use_builtin_process_launcher: false,
                security_policy: SecurityPolicy {
                    job_policy: JobPolicyAllowlists {
                        ambient_mark_vmo_exec: vec![
                            AllowlistEntryBuilder::new().build(),
                            AllowlistEntryBuilder::new().exact("foo").exact("bar").build(),
                        ],
                        main_process_critical: vec![
                            AllowlistEntryBuilder::new().exact("something").exact("important").build(),
                        ],
                        create_raw_processes: vec![
                            AllowlistEntryBuilder::new().exact("another").exact("thing").build(),
                        ],
                    },
                    capability_policy: HashMap::from_iter(vec![
                        (CapabilityAllowlistKey {
                            source_moniker: ExtendedMoniker::ComponentManager,
                            source_name: "fuchsia.boot.RootResource".parse().unwrap(),
                            source: CapabilityAllowlistSource::Self_,
                            capability: CapabilityTypeName::Protocol,
                        },
                        HashSet::from_iter(vec![
                            AllowlistEntryBuilder::new().exact("bootstrap").build(),
                            AllowlistEntryBuilder::new().exact("core").any_descendant(),
                            AllowlistEntryBuilder::new().exact("core").exact("test_manager").any_descendant_in_collection("tests"),
                        ].iter().cloned())
                        ),
                    ].iter().cloned()),
                    debug_capability_policy: HashMap::from_iter(vec![
                        (
                            DebugCapabilityKey {
                                source_name: "fuchsia.foo.bar".parse().unwrap(),
                                source: CapabilityAllowlistSource::Self_,
                                capability: CapabilityTypeName::Protocol,
                                env_name: "bar_env1".to_string(),
                            },
                            HashSet::from_iter(vec![
                                DebugCapabilityAllowlistEntry::new(
                                    AllowlistEntryBuilder::new().exact("foo").exact("bar").exact("baz").build(),
                                    AllowlistEntryBuilder::new().exact("foo").exact("bar").build(),
                                )
                            ])
                        ),
                        (
                            DebugCapabilityKey {
                                source_name: "fuchsia.foo.bar".parse().unwrap(),
                                source: CapabilityAllowlistSource::Self_,
                                capability: CapabilityTypeName::Protocol,
                                env_name: "foo_env1".to_string(),
                            },
                            HashSet::from_iter(vec![
                                DebugCapabilityAllowlistEntry::new(
                                    AllowlistEntryBuilder::new().exact("foo").exact("bar").exact("baz").build(),
                                    AllowlistEntryBuilder::new().exact("foo").build(),
                                )
                            ])
                        ),
                        (
                            DebugCapabilityKey {
                                source_name: "fuchsia.foo.baz".parse().unwrap(),
                                source: CapabilityAllowlistSource::Self_,
                                capability: CapabilityTypeName::Protocol,
                                env_name: "foo_env2".to_string(),
                            },
                            HashSet::from_iter(vec![
                                DebugCapabilityAllowlistEntry::new(
                                    AllowlistEntryBuilder::new().exact("foo").exact("bar").any_descendant(),
                                    AllowlistEntryBuilder::new().exact("foo").any_descendant(),
                                )
                            ])
                        ),
                        (
                            DebugCapabilityKey {
                                source_name: "fuchsia.foo.baz".parse().unwrap(),
                                source: CapabilityAllowlistSource::Self_,
                                capability: CapabilityTypeName::Protocol,
                                env_name: "root_env".to_string(),
                            },
                            HashSet::from_iter(vec![
                                DebugCapabilityAllowlistEntry::new(
                                    AllowlistEntryBuilder::new().exact("foo").exact("bar").any_descendant_in_collection("coll"),
                                    AllowlistEntryBuilder::new().exact("root").build(),
                                )
                            ])
                        ),
                    ]),
                    child_policy: ChildPolicyAllowlists {
                        reboot_on_terminate: vec![
                            AllowlistEntryBuilder::new().exact("something").exact("important").build(),
                        ],
                    },
                },
                num_threads: 24,
                namespace_capabilities: vec![
                    cm_rust::CapabilityDecl::Protocol(cm_rust::ProtocolDecl {
                        name: "foo_svc".parse().unwrap(),
                        source_path: Some("/svc/foo".parse().unwrap()),
                    }),
                    cm_rust::CapabilityDecl::Directory(cm_rust::DirectoryDecl {
                        name: "bar_dir".parse().unwrap(),
                        source_path: Some("/bar".parse().unwrap()),
                        rights: fio::Operations::CONNECT,
                    }),
                ],
                builtin_capabilities: vec![
                    cm_rust::CapabilityDecl::Protocol(cm_rust::ProtocolDecl {
                        name: "foo_protocol".parse().unwrap(),
                        source_path: None,
                    }),
                ],
                root_component_url: Some(Url::new(FOO_PKG_URL.to_string()).unwrap()),
                component_id_index_path: Some("/boot/config/component_id_index".to_string()),
                log_destination: LogDestination::Klog,
                log_all_events: true,
                builtin_boot_resolver: BuiltinBootResolver::None,
                reboot_on_terminate_enabled: true,
                realm_builder_resolver_and_runner: RealmBuilderResolverAndRunner::None,
            }
        ),
    }

    test_config_err! {
        invalid_job_policy => (
            component_internal::Config {
                debug: None,
                enable_introspection: None,
                list_children_batch_size: None,
                maintain_utc_clock: None,
                use_builtin_process_launcher: None,
                security_policy: Some(component_internal::SecurityPolicy {
                    job_policy: Some(component_internal::JobPolicyAllowlists {
                        main_process_critical: None,
                        ambient_mark_vmo_exec: Some(vec!["/".to_string(), "bad".to_string()]),
                        create_raw_processes: None,
                        ..Default::default()
                    }),
                    capability_policy: None,
                    ..Default::default()
                }),
                num_threads: None,
                namespace_capabilities: None,
                builtin_capabilities: None,
                root_component_url: None,
                component_id_index_path: None,
                reboot_on_terminate_enabled: None,
                ..Default::default()
            },
            AllowlistEntryParseError,
            AllowlistEntryParseError::NoLeadingSlash(
                "bad".into(),
            )
        ),
        invalid_capability_policy_empty_allowlist_cap => (
            component_internal::Config {
                debug: None,
                enable_introspection: None,
                list_children_batch_size: None,
                maintain_utc_clock: None,
                use_builtin_process_launcher: None,
                security_policy: Some(component_internal::SecurityPolicy {
                    job_policy: None,
                    capability_policy: Some(component_internal::CapabilityPolicyAllowlists {
                        allowlist: Some(vec![
                        component_internal::CapabilityAllowlistEntry {
                            source_moniker: Some("<component_manager>".to_string()),
                            source_name: Some("fuchsia.boot.RootResource".to_string()),
                            source: Some(fdecl::Ref::Self_(fdecl::SelfRef{})),
                            capability: None,
                            target_monikers: Some(vec!["/core".to_string()]),
                            ..Default::default()
                        }]),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                num_threads: None,
                namespace_capabilities: None,
                builtin_capabilities: None,
                root_component_url: None,
                component_id_index_path: None,
                ..Default::default()
            },
            PolicyConfigError,
            PolicyConfigError::EmptyAllowlistedCapability
        ),
        invalid_capability_policy_empty_source_moniker => (
            component_internal::Config {
                debug: None,
                enable_introspection: None,
                list_children_batch_size: None,
                maintain_utc_clock: None,
                use_builtin_process_launcher: None,
                security_policy: Some(component_internal::SecurityPolicy {
                    job_policy: None,
                    capability_policy: Some(component_internal::CapabilityPolicyAllowlists {
                        allowlist: Some(vec![
                        component_internal::CapabilityAllowlistEntry {
                            source_moniker: None,
                            source_name: Some("fuchsia.boot.RootResource".to_string()),
                            capability: Some(component_internal::AllowlistedCapability::Protocol(component_internal::AllowlistedProtocol::default())),
                            target_monikers: Some(vec!["/core".to_string()]),
                            ..Default::default()
                        }]),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                num_threads: None,
                namespace_capabilities: None,
                builtin_capabilities: None,
                root_component_url: None,
                component_id_index_path: None,
                reboot_on_terminate_enabled: None,
                ..Default::default()
            },
            PolicyConfigError,
            PolicyConfigError::EmptySourceMoniker
        ),
        invalid_root_component_url => (
            component_internal::Config {
                debug: None,
                enable_introspection: None,
                list_children_batch_size: None,
                maintain_utc_clock: None,
                use_builtin_process_launcher: None,
                security_policy: None,
                num_threads: None,
                namespace_capabilities: None,
                builtin_capabilities: None,
                root_component_url: Some("invalid url".to_string()),
                component_id_index_path: None,
                reboot_on_terminate_enabled: None,
                ..Default::default()
            },
            ParseError,
            ParseError::InvalidComponentUrl {
                details: String::from("Relative URL has no resource fragment.")
            }
        ),
    }

    fn write_config_to_file(
        tmp_dir: &TempDir,
        config: component_internal::Config,
    ) -> Result<PathBuf, Error> {
        let path = tmp_dir.path().join("test_config.fidl");
        let content = fidl::persist(&config)?;
        std::fs::write(&path, &content)?;
        Ok(path)
    }

    #[test]
    fn config_from_file_no_arg() -> Result<(), Error> {
        assert_matches!(RuntimeConfig::load_from_file::<PathBuf>(Default::default()), Err(_));
        Ok(())
    }

    #[test]
    fn config_from_file_missing() -> Result<(), Error> {
        let path = PathBuf::from(&"/foo/bar".to_string());
        assert_matches!(RuntimeConfig::load_from_file(&path), Err(_));
        Ok(())
    }

    #[test]
    fn config_from_file_valid() -> Result<(), Error> {
        let tempdir = TempDir::new().expect("failed to create temp directory");
        let path = write_config_to_file(
            &tempdir,
            component_internal::Config {
                debug: None,
                enable_introspection: None,
                list_children_batch_size: Some(42),
                security_policy: None,
                namespace_capabilities: None,
                builtin_capabilities: None,
                maintain_utc_clock: None,
                use_builtin_process_launcher: None,
                num_threads: None,
                root_component_url: Some(FOO_PKG_URL.to_string()),
                ..Default::default()
            },
        )?;
        let expected = RuntimeConfig {
            list_children_batch_size: 42,
            root_component_url: Some(Url::new(FOO_PKG_URL.to_string())?),
            ..Default::default()
        };

        assert_matches!(
            RuntimeConfig::load_from_file(&path)
            , Ok(v) if v == expected);
        Ok(())
    }

    #[test]
    fn config_from_file_invalid() -> Result<(), Error> {
        let tempdir = TempDir::new().expect("failed to create temp directory");
        let path = tempdir.path().join("test_config.fidl");
        // Add config file containing garbage data.
        std::fs::write(&path, &vec![0xfa, 0xde])?;

        assert_matches!(RuntimeConfig::load_from_file(&path), Err(_));
        Ok(())
    }

    #[test]
    fn abi_revision_policy_check_compatibility() -> Result<(), Error> {
        // This test assumes the platform does not support a u64::MAX ABI value.
        let invalid_abi = AbiRevision(u64::MAX);
        let valid_abi = version_history::LATEST_VERSION.abi_revision;
        let supported_abis: Vec<_> = version_history::get_supported_abi_revisions()
            .into_iter()
            .map(|v| format!("{}", version_history::AbiRevision(v)))
            .collect();
        let test_scenarios = vec![
            (AbiRevisionPolicy::AllowAll, None, Ok(())),
            (AbiRevisionPolicy::AllowAll, Some(invalid_abi), Ok(())),
            (AbiRevisionPolicy::AllowAll, Some(valid_abi), Ok(())),
            (AbiRevisionPolicy::EnforcePresenceOnly, None, Err(AbiRevisionError::Absent)),
            (AbiRevisionPolicy::EnforcePresenceOnly, Some(invalid_abi), Ok(())),
            (AbiRevisionPolicy::EnforcePresenceOnly, Some(valid_abi), Ok(())),
            (
                AbiRevisionPolicy::EnforcePresenceAndCompatibility,
                None,
                Err(AbiRevisionError::Absent),
            ),
            (
                AbiRevisionPolicy::EnforcePresenceAndCompatibility,
                Some(invalid_abi),
                Err(AbiRevisionError::Unsupported(invalid_abi, supported_abis.join(","))),
            ),
            (AbiRevisionPolicy::EnforcePresenceAndCompatibility, Some(valid_abi), Ok(())),
        ];
        for (policy, abi, expected_res) in test_scenarios {
            println!(
                "Test {:?} policy against the ABI revision {:?} produces {:?} result",
                policy, abi, expected_res
            );
            let res = policy.check_compatibility(&"/foo".try_into().unwrap(), abi);
            assert_eq!(res, expected_res);
        }
        Ok(())
    }

    macro_rules! test_entries_ok {
        ( $($test_name:ident => ($input:expr, $expected:expr)),+ $(,)? ) => {
            test_function_ok! { parse_allowlist_entries, $($test_name => ($input, $expected)),+ }
        };
    }

    macro_rules! test_entries_err {
        ( $($test_name:ident => ($input:expr, $type:ty, $expected:expr)),+ $(,)? ) => {
            test_function_err! { parse_allowlist_entries, $($test_name => ($input, $type, $expected)),+ }
        };
    }

    test_entries_ok! {
        missing_entries => (&None, vec![]),
        empty_entries => (&Some(vec![]), vec![]),
        all_entry_types => (&Some(vec![
            "/core".into(),
            "/**".into(),
            "/foo/**".into(),
            "/coll:**".into(),
            "/core/test_manager/tests:**".into(),
            "/core/ffx-laboratory:*/echo_client".into(),
            "/core/*/ffx-laboratory:*/**".into(),
            "/core/*/bar".into(),
        ]), vec![
            AllowlistEntryBuilder::new().exact("core").build(),
            AllowlistEntryBuilder::new().any_descendant(),
            AllowlistEntryBuilder::new().exact("foo").any_descendant(),
            AllowlistEntryBuilder::new().any_descendant_in_collection("coll"),
            AllowlistEntryBuilder::new().exact("core").exact("test_manager").any_descendant_in_collection("tests"),
            AllowlistEntryBuilder::new().exact("core").any_child_in_collection("ffx-laboratory").exact("echo_client").build(),
            AllowlistEntryBuilder::new().exact("core").any_child().any_child_in_collection("ffx-laboratory").any_descendant(),
            AllowlistEntryBuilder::new().exact("core").any_child().exact("bar").build(),
        ])
    }

    test_entries_err! {
        invalid_realm_entry => (
            &Some(vec!["/foo/**".into(), "bar/**".into()]),
            AllowlistEntryParseError,
            AllowlistEntryParseError::NoLeadingSlash("bar/**".into())),
        invalid_realm_in_collection_entry => (
            &Some(vec!["/foo/coll:**".into(), "bar/coll:**".into()]),
            AllowlistEntryParseError,
            AllowlistEntryParseError::NoLeadingSlash("bar/coll:**".into())),
        missing_realm_in_collection_entry => (
            &Some(vec!["coll:**".into()]),
            AllowlistEntryParseError,
            AllowlistEntryParseError::NoLeadingSlash("coll:**".into())),
        missing_collection_name => (
            &Some(vec!["/foo/coll:**".into(), "/:**".into()]),
            AllowlistEntryParseError,
            AllowlistEntryParseError::InvalidCollectionName(
                "".into(),
                ParseError::Empty
            )),
        invalid_collection_name => (
            &Some(vec!["/foo/coll:**".into(), "/*:**".into()]),
            AllowlistEntryParseError,
            AllowlistEntryParseError::InvalidCollectionName(
                "*".into(),
                ParseError::InvalidValue
            )),
        invalid_exact_entry => (
            &Some(vec!["/foo/bar*".into()]),
            AllowlistEntryParseError,
            AllowlistEntryParseError::InvalidChildMoniker(
                "bar*".into(),
                MonikerError::InvalidMonikerPart { 0: ParseError::InvalidValue }
            )),
        descendant_wildcard_in_between => (
            &Some(vec!["/foo/**/bar".into()]),
            AllowlistEntryParseError,
            AllowlistEntryParseError::DescendantWildcardOnlyAtEnd(
                "/foo/**/bar".into(),
            )),
    }
}
