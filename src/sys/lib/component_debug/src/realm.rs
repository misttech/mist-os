// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::io::Directory,
    anyhow::Context,
    cm_rust::{ComponentDecl, FidlIntoNative},
    fidl::endpoints::{create_proxy, ServerEnd},
    fidl_fuchsia_component_decl as fcdecl, fidl_fuchsia_io as fio, fidl_fuchsia_sys2 as fsys,
    fuchsia_async::TimeoutExt,
    futures::TryFutureExt,
    moniker::{
        AbsoluteMoniker, AbsoluteMonikerBase, MonikerError, RelativeMoniker, RelativeMonikerBase,
    },
    thiserror::Error,
};

/// This value is somewhat arbitrarily chosen based on how long we expect a component to take to
/// respond to a directory request. There is no clear answer for how long it should take a
/// component to respond. A request may take unnaturally long if the host is connected to the
/// target over a weak network connection. The target may be busy doing other work, resulting in a
/// delayed response here. A request may never return a response, if the component is simply holding
/// onto the directory handle without serving or dropping it. We should choose a value that balances
/// a reasonable expectation from the component without making the user wait for too long.
// TODO(http://fxbug.dev/99927): Get network latency info from ffx to choose a better timeout.
static DIR_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

#[cfg(feature = "serde")]
use serde::Serialize;

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("{struct_name} FIDL is missing a field: {field_name}")]
    MissingField { struct_name: &'static str, field_name: &'static str },

    #[error("moniker could not be parsed successfully: {0}")]
    BadMoniker(#[from] MonikerError),

    #[error("{struct_name} FIDL enum is set to an unknown value")]
    UnknownEnumValue { struct_name: &'static str },
}

#[derive(Debug, Error)]
pub enum GetInstanceError {
    #[error("instance {0} could not be found")]
    InstanceNotFound(AbsoluteMoniker),

    #[error("component manager could not parse {0}")]
    BadMoniker(AbsoluteMoniker),

    #[error(transparent)]
    ParseError(#[from] ParseError),

    #[error("component manager responded with an unknown error code")]
    UnknownError,

    #[error(transparent)]
    Fidl(#[from] fidl::Error),
}

#[derive(Debug, Error)]
pub enum GetAllInstancesError {
    #[error("scoped root instance could not be found")]
    InstanceNotFound,

    #[error(transparent)]
    ParseError(#[from] ParseError),

    #[error("component manager responded with an unknown error code")]
    UnknownError,

    #[error("FIDL error: {0}")]
    Fidl(#[from] fidl::Error),
}

#[derive(Debug, Error)]
pub enum GetRuntimeError {
    #[error(transparent)]
    Fidl(#[from] fidl::Error),

    #[error("Component manager could not open runtime dir: {0:?}")]
    OpenError(fsys::OpenError),

    #[error("timed out parsing dir")]
    Timeout,

    #[error("error parsing dir: {0}")]
    ParseError(#[source] anyhow::Error),
}

#[derive(Debug, Error)]
pub enum GetOutgoingCapabilitiesError {
    #[error(transparent)]
    Fidl(#[from] fidl::Error),

    #[error("Component manager could not open outgoing dir: {0:?}")]
    OpenError(fsys::OpenError),

    #[error("timed out parsing dir")]
    Timeout,

    #[error("error parsing dir: {0}")]
    ParseError(#[source] anyhow::Error),
}

#[derive(Debug, Error)]
pub enum GetMerkleRootError {
    #[error(transparent)]
    Fidl(#[from] fidl::Error),

    #[error("Component manager could not open pacakage dir: {0:?}")]
    OpenError(fsys::OpenError),

    #[error("error reading meta file: {0}")]
    ReadError(#[from] fuchsia_fs::file::ReadError),
}

#[derive(Debug, Error)]
pub enum GetManifestError {
    #[error(transparent)]
    Fidl(#[from] fidl::Error),

    #[error("instance {0} could not be found")]
    InstanceNotFound(AbsoluteMoniker),

    #[error("instance {0} is not resolved")]
    InstanceNotResolved(AbsoluteMoniker),

    #[error("component manager could not parse {0}")]
    BadMoniker(AbsoluteMoniker),

    #[error("component manager failed to encode the manifest")]
    EncodeFailed,

    #[error("component does not have {_0:?} as a valid location for a child")]
    BadChildLocation(fsys::ChildLocation),

    #[error("could not resolve component from URL {_0}")]
    BadUrl(String),

    #[error("component manifest could not be validated")]
    InvalidManifest(#[from] cm_fidl_validator::error::ErrorList),

    #[error("component manager responded with an unknown error code")]
    UnknownError,
}

#[derive(Debug, Error)]
pub enum GetStructuredConfigError {
    #[error(transparent)]
    Fidl(#[from] fidl::Error),

    #[error(transparent)]
    ParseError(#[from] ParseError),

    #[error("instance {0} could not be found")]
    InstanceNotFound(AbsoluteMoniker),

    #[error("instance {0} is not resolved")]
    InstanceNotResolved(AbsoluteMoniker),

    #[error("component manager could not parse {0}")]
    BadMoniker(AbsoluteMoniker),

    #[error("component manager responded with an unknown error code")]
    UnknownError,
}

#[cfg_attr(feature = "serde", derive(Serialize))]
#[derive(Debug)]
pub enum InstanceType {
    Cml,
    Cmx(#[cfg_attr(feature = "serde", serde(skip))] Directory),
}

impl std::fmt::Display for InstanceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cml => write!(f, "CML component"),
            Self::Cmx(_) => write!(f, "CMX component"),
        }
    }
}

#[cfg_attr(feature = "serde", derive(Serialize))]
#[derive(Debug)]
pub struct Instance {
    /// Moniker of the component.
    pub moniker: AbsoluteMoniker,

    /// URL of the component.
    pub url: String,

    /// Environment of the component from its parent.
    pub environment: Option<String>,

    /// Unique identifier of component.
    pub instance_id: Option<String>,

    /// Type of instance.
    // TODO(https://fxbug.dev/102390): Remove this when CMX is deprecated.
    pub instance_type: InstanceType,

    /// Information about resolved state of instance.
    pub resolved_info: Option<ResolvedInfo>,
}

impl TryFrom<fsys::Instance> for Instance {
    type Error = ParseError;

    fn try_from(instance: fsys::Instance) -> Result<Self, Self::Error> {
        let moniker = instance
            .moniker
            .ok_or(ParseError::MissingField { struct_name: "Instance", field_name: "moniker" })?;
        let moniker = RelativeMoniker::parse_str(&moniker)?;
        let moniker = AbsoluteMoniker::root().descendant(&moniker);
        let url = instance
            .url
            .ok_or(ParseError::MissingField { struct_name: "Instance", field_name: "url" })?;
        let resolved_info = instance.resolved_info.map(|i| i.try_into()).transpose()?;

        Ok(Self {
            moniker,
            url,
            environment: instance.environment,
            instance_id: instance.instance_id,
            instance_type: InstanceType::Cml,
            resolved_info,
        })
    }
}

/// Additional information about components that are resolved.
#[cfg_attr(feature = "serde", derive(Serialize))]
#[derive(Debug)]
pub struct ResolvedInfo {
    pub resolved_url: String,
    pub execution_info: Option<ExecutionInfo>,
}

impl TryFrom<fsys::ResolvedInfo> for ResolvedInfo {
    type Error = ParseError;

    fn try_from(resolved: fsys::ResolvedInfo) -> Result<Self, Self::Error> {
        let resolved_url = resolved.resolved_url.ok_or(ParseError::MissingField {
            struct_name: "ResolvedInfo",
            field_name: "resolved_url",
        })?;
        let execution_info = resolved.execution_info.map(|i| i.try_into()).transpose()?;

        Ok(Self { resolved_url, execution_info })
    }
}

/// Additional information about components that are running.
#[cfg_attr(feature = "serde", derive(Serialize))]
#[derive(Debug)]
pub struct ExecutionInfo {
    pub start_reason: String,
}

impl TryFrom<fsys::ExecutionInfo> for ExecutionInfo {
    type Error = ParseError;

    fn try_from(info: fsys::ExecutionInfo) -> Result<Self, Self::Error> {
        let start_reason = info.start_reason.ok_or(ParseError::MissingField {
            struct_name: "ExecutionInfo",
            field_name: "start_reason",
        })?;
        Ok(Self { start_reason })
    }
}

/// A single structured configuration key-value pair.
#[cfg_attr(feature = "serde", derive(Serialize))]
#[derive(Debug, PartialEq)]
pub struct ConfigField {
    pub key: String,
    pub value: String,
}

impl TryFrom<fcdecl::ResolvedConfigField> for ConfigField {
    type Error = ParseError;

    fn try_from(field: fcdecl::ResolvedConfigField) -> Result<Self, Self::Error> {
        let value = match &field.value {
            fcdecl::ConfigValue::Vector(value) => format!("{:#?}", value),
            fcdecl::ConfigValue::Single(value) => format!("{:?}", value),
            _ => {
                return Err(ParseError::UnknownEnumValue {
                    struct_name: "fuchsia.component.config.Value",
                })
            }
        };
        Ok(ConfigField { key: field.key.clone(), value })
    }
}

#[cfg_attr(feature = "serde", derive(Serialize))]
#[derive(Debug)]
pub enum Runtime {
    Elf {
        job_id: u64,
        process_id: Option<u64>,
        process_start_time: Option<i64>,
        process_start_time_utc_estimate: Option<String>,
    },
    Unknown,
}

#[cfg_attr(feature = "serde", derive(Serialize))]
#[derive(Debug, PartialEq)]
pub enum Durability {
    Transient,
    SingleRun,
}

impl std::fmt::Display for Durability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transient => write!(f, "Transient"),
            Self::SingleRun => write!(f, "Single-run"),
        }
    }
}

impl From<fcdecl::Durability> for Durability {
    fn from(value: fcdecl::Durability) -> Self {
        match value {
            fcdecl::Durability::SingleRun => Durability::SingleRun,
            fcdecl::Durability::Transient => Durability::Transient,
        }
    }
}

pub async fn get_all_instances(
    query: &fsys::RealmQueryProxy,
) -> Result<Vec<Instance>, GetAllInstancesError> {
    let result = query.get_all_instances().await?;

    let iterator = match result {
        Ok(iterator) => iterator,
        Err(fsys::GetAllInstancesError::InstanceNotFound) => {
            return Err(GetAllInstancesError::InstanceNotFound)
        }
        Err(_) => return Err(GetAllInstancesError::UnknownError),
    };

    let iterator = iterator.into_proxy().unwrap();
    let mut instances = vec![];

    loop {
        let mut batch = iterator.next().await?;
        if batch.is_empty() {
            break;
        }
        instances.append(&mut batch);
    }

    let instances: Result<Vec<Instance>, ParseError> =
        instances.into_iter().map(|i| Instance::try_from(i)).collect();
    Ok(instances?)
}

pub async fn get_manifest(
    moniker: &AbsoluteMoniker,
    realm_query: &fsys::RealmQueryProxy,
) -> Result<ComponentDecl, GetManifestError> {
    let moniker_str = format!(".{}", moniker.to_string());
    let iterator = match realm_query.get_manifest(&moniker_str).await? {
        Ok(iterator) => Ok(iterator),
        Err(fsys::GetManifestError::InstanceNotFound) => {
            Err(GetManifestError::InstanceNotFound(moniker.clone()))
        }
        Err(fsys::GetManifestError::InstanceNotResolved) => {
            Err(GetManifestError::InstanceNotResolved(moniker.clone()))
        }
        Err(fsys::GetManifestError::BadMoniker) => {
            Err(GetManifestError::BadMoniker(moniker.clone()))
        }
        Err(fsys::GetManifestError::EncodeFailed) => Err(GetManifestError::EncodeFailed),
        Err(_) => Err(GetManifestError::UnknownError),
    }?;

    let bytes = drain_manifest_bytes_iterator(iterator.into_proxy().unwrap()).await?;
    let manifest = fidl::unpersist::<fcdecl::Component>(&bytes)?;
    let manifest = manifest.fidl_into_native();
    Ok(manifest)
}

async fn drain_manifest_bytes_iterator(
    iterator: fsys::ManifestBytesIteratorProxy,
) -> Result<Vec<u8>, fidl::Error> {
    let mut bytes = vec![];

    loop {
        let mut batch = iterator.next().await?;
        if batch.is_empty() {
            break;
        }
        bytes.append(&mut batch);
    }

    Ok(bytes)
}

pub async fn resolve_declaration(
    realm_query: &fsys::RealmQueryProxy,
    parent: &AbsoluteMoniker,
    child_location: &fsys::ChildLocation,
    url: &str,
) -> Result<ComponentDecl, GetManifestError> {
    let iterator = realm_query
        .resolve_declaration(&format!(".{}", parent), child_location, url)
        .await?
        .map_err(|e| match e {
            fsys::GetManifestError::InstanceNotFound => {
                GetManifestError::InstanceNotFound(parent.clone())
            }
            fsys::GetManifestError::InstanceNotResolved => {
                GetManifestError::InstanceNotResolved(parent.clone())
            }
            fsys::GetManifestError::BadMoniker => GetManifestError::BadMoniker(parent.clone()),
            fsys::GetManifestError::EncodeFailed => GetManifestError::EncodeFailed,
            fsys::GetManifestError::BadChildLocation => {
                GetManifestError::BadChildLocation(child_location.to_owned())
            }
            fsys::GetManifestError::BadUrl => GetManifestError::BadUrl(url.to_owned()),
            _ => GetManifestError::UnknownError,
        })?;

    let bytes = drain_manifest_bytes_iterator(iterator.into_proxy().unwrap()).await?;
    let manifest = fidl::unpersist::<fcdecl::Component>(&bytes)?;
    cm_fidl_validator::validate(&manifest)?;
    let manifest = manifest.fidl_into_native();
    Ok(manifest)
}

pub async fn get_config_fields(
    moniker: &AbsoluteMoniker,
    realm_query: &fsys::RealmQueryProxy,
) -> Result<Option<Vec<ConfigField>>, GetStructuredConfigError> {
    // Parse the runtime directory and add it into the State object
    let moniker_str = format!(".{}", moniker.to_string());
    match realm_query.get_structured_config(&moniker_str).await? {
        Ok(config) => {
            let fields: Result<Vec<ConfigField>, ParseError> =
                config.fields.into_iter().map(|f| f.try_into()).collect();
            let fields = fields?;
            Ok(Some(fields))
        }
        Err(fsys::GetStructuredConfigError::InstanceNotFound) => {
            Err(GetStructuredConfigError::InstanceNotFound(moniker.clone()))
        }
        Err(fsys::GetStructuredConfigError::InstanceNotResolved) => {
            Err(GetStructuredConfigError::InstanceNotResolved(moniker.clone()))
        }
        Err(fsys::GetStructuredConfigError::NoConfig) => Ok(None),
        Err(fsys::GetStructuredConfigError::BadMoniker) => {
            Err(GetStructuredConfigError::BadMoniker(moniker.clone()))
        }
        Err(_) => Err(GetStructuredConfigError::UnknownError),
    }
}

pub async fn get_runtime(
    moniker: &AbsoluteMoniker,
    realm_query: &fsys::RealmQueryProxy,
) -> Result<Runtime, GetRuntimeError> {
    // Parse the runtime directory and add it into the State object
    let moniker_str = format!(".{}", moniker.to_string());
    let (runtime_dir, server_end) = create_proxy::<fio::DirectoryMarker>().unwrap();
    let runtime_dir = Directory::from_proxy(runtime_dir);
    let server_end = ServerEnd::new(server_end.into_channel());
    realm_query
        .open(
            &moniker_str,
            fsys::OpenDirType::RuntimeDir,
            fio::OpenFlags::RIGHT_READABLE,
            fio::ModeType::empty(),
            ".",
            server_end,
        )
        .await?
        .map_err(|e| GetRuntimeError::OpenError(e))?;
    parse_runtime_from_dir(runtime_dir)
        .map_err(|e| GetRuntimeError::ParseError(e))
        .on_timeout(DIR_TIMEOUT, || Err(GetRuntimeError::Timeout))
        .await
}

async fn parse_runtime_from_dir(runtime_dir: Directory) -> Result<Runtime, anyhow::Error> {
    // Some runners may not serve the runtime directory, so attempting to get the entries
    // may fail. This is normal and should be treated as no ELF runtime.
    if let Ok(true) = runtime_dir.exists("elf").await {
        let elf_dir = runtime_dir.open_dir_readable("elf")?;

        let (job_id, process_id, process_start_time, process_start_time_utc_estimate) = futures::join!(
            elf_dir.read_file("job_id"),
            elf_dir.read_file("process_id"),
            elf_dir.read_file("process_start_time"),
            elf_dir.read_file("process_start_time_utc_estimate"),
        );

        let job_id = job_id?.parse::<u64>().context("Job ID is not u64")?;

        let process_id = match process_id {
            Ok(id) => Some(id.parse::<u64>().context("Process ID is not u64")?),
            Err(_) => None,
        };

        let process_start_time =
            process_start_time.ok().map(|time_string| time_string.parse::<i64>().ok()).flatten();

        let process_start_time_utc_estimate = process_start_time_utc_estimate.ok();

        Ok(Runtime::Elf { job_id, process_id, process_start_time, process_start_time_utc_estimate })
    } else {
        Ok(Runtime::Unknown)
    }
}

pub async fn get_instance(
    moniker: &AbsoluteMoniker,
    realm_query: &fsys::RealmQueryProxy,
) -> Result<Instance, GetInstanceError> {
    let moniker_str = format!(".{}", moniker.to_string());
    match realm_query.get_instance(&moniker_str).await? {
        Ok(instance) => {
            let instance = instance.try_into()?;
            Ok(instance)
        }
        Err(fsys::GetInstanceError::InstanceNotFound) => {
            Err(GetInstanceError::InstanceNotFound(moniker.clone()))
        }
        Err(fsys::GetInstanceError::BadMoniker) => {
            Err(GetInstanceError::BadMoniker(moniker.clone()))
        }
        Err(_) => Err(GetInstanceError::UnknownError),
    }
}

pub async fn get_outgoing_capabilities(
    moniker: &AbsoluteMoniker,
    realm_query: &fsys::RealmQueryProxy,
) -> Result<Vec<String>, GetOutgoingCapabilitiesError> {
    let moniker_str = format!(".{}", moniker.to_string());
    let (out_dir, server_end) = create_proxy::<fio::DirectoryMarker>().unwrap();
    let out_dir = Directory::from_proxy(out_dir);
    let server_end = ServerEnd::new(server_end.into_channel());
    realm_query
        .open(
            &moniker_str,
            fsys::OpenDirType::OutgoingDir,
            fio::OpenFlags::RIGHT_READABLE,
            fio::ModeType::empty(),
            ".",
            server_end,
        )
        .await?
        .map_err(|e| GetOutgoingCapabilitiesError::OpenError(e))?;
    get_capabilities(out_dir)
        .map_err(|e| GetOutgoingCapabilitiesError::ParseError(e))
        .on_timeout(DIR_TIMEOUT, || Err(GetOutgoingCapabilitiesError::Timeout))
        .await
}

pub async fn get_merkle_root(
    moniker: &AbsoluteMoniker,
    realm_query: &fsys::RealmQueryProxy,
) -> Result<String, GetMerkleRootError> {
    let moniker_str = format!(".{}", moniker.to_string());
    let (meta_file, server_end) = create_proxy::<fio::FileMarker>().unwrap();
    let server_end = ServerEnd::new(server_end.into_channel());
    realm_query
        .open(
            &moniker_str,
            fsys::OpenDirType::PackageDir,
            fio::OpenFlags::RIGHT_READABLE,
            fio::ModeType::empty(),
            "meta",
            server_end,
        )
        .await?
        .map_err(|e| GetMerkleRootError::OpenError(e))?;
    let merkle_root = fuchsia_fs::file::read_to_string(&meta_file).await?;
    Ok(merkle_root)
}

// Get all entries in a capabilities directory. If there is a "svc" directory, traverse it and
// collect all protocol names as well.
async fn get_capabilities(capability_dir: Directory) -> Result<Vec<String>, anyhow::Error> {
    let mut entries = capability_dir.entry_names().await?;

    for (index, name) in entries.iter().enumerate() {
        if name == "svc" {
            entries.remove(index);
            let svc_dir = capability_dir.open_dir_readable("svc")?;
            let mut svc_entries = svc_dir.entry_names().await?;
            entries.append(&mut svc_entries);
            break;
        }
    }

    entries.sort_unstable();
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::*;
    use fidl_fuchsia_component_decl as fdecl;
    use std::collections::HashMap;
    use tempfile::TempDir;

    #[fuchsia::test]
    async fn test_get_all_instances() {
        let query = serve_realm_query_instances(vec![fsys::Instance {
            moniker: Some("./my_foo".to_string()),
            url: Some("#meta/foo.cm".to_string()),
            instance_id: Some("1234567890".to_string()),
            resolved_info: Some(fsys::ResolvedInfo {
                resolved_url: Some("fuchsia-pkg://fuchsia.com/foo#meta/foo.cm".to_string()),
                execution_info: Some(fsys::ExecutionInfo {
                    start_reason: Some("Debugging Workflow".to_string()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        }]);

        let mut instances = get_all_instances(&query).await.unwrap();
        assert_eq!(instances.len(), 1);
        let instance = instances.remove(0);

        let moniker = AbsoluteMoniker::parse_str("/my_foo").unwrap();
        assert_eq!(instance.moniker, moniker);
        assert_eq!(instance.url, "#meta/foo.cm");
        assert_eq!(instance.instance_id.unwrap(), "1234567890");
        assert!(instance.resolved_info.is_some());

        let resolved = instance.resolved_info.unwrap();
        assert_eq!(resolved.resolved_url, "fuchsia-pkg://fuchsia.com/foo#meta/foo.cm");

        let execution_info = resolved.execution_info.unwrap();
        assert_eq!(execution_info.start_reason, "Debugging Workflow".to_string());
    }

    #[fuchsia::test]
    async fn test_get_manifest() {
        let query = serve_realm_query(
            vec![],
            HashMap::from([(
                "./my_foo".to_string(),
                fdecl::Component {
                    uses: Some(vec![fdecl::Use::Protocol(fdecl::UseProtocol {
                        source: Some(fdecl::Ref::Parent(fdecl::ParentRef)),
                        source_name: Some("fuchsia.foo.bar".to_string()),
                        target_path: Some("/svc/fuchsia.foo.bar".to_string()),
                        dependency_type: Some(fdecl::DependencyType::Strong),
                        availability: Some(fdecl::Availability::Required),
                        ..Default::default()
                    })]),
                    exposes: Some(vec![fdecl::Expose::Protocol(fdecl::ExposeProtocol {
                        source: Some(fdecl::Ref::Self_(fdecl::SelfRef)),
                        source_name: Some("fuchsia.bar.baz".to_string()),
                        target: Some(fdecl::Ref::Parent(fdecl::ParentRef)),
                        target_name: Some("fuchsia.bar.baz".to_string()),
                        ..Default::default()
                    })]),
                    capabilities: Some(vec![fdecl::Capability::Protocol(fdecl::Protocol {
                        name: Some("fuchsia.bar.baz".to_string()),
                        source_path: Some("/svc/fuchsia.bar.baz".to_string()),
                        ..Default::default()
                    })]),
                    ..Default::default()
                },
            )]),
            HashMap::new(),
            HashMap::new(),
        );

        let moniker = AbsoluteMoniker::parse_str("/my_foo").unwrap();
        let manifest = get_manifest(&moniker, &query).await.unwrap();

        assert_eq!(manifest.uses.len(), 1);
        assert_eq!(manifest.exposes.len(), 1);
    }

    pub fn create_pkg_dir() -> TempDir {
        let temp_dir = TempDir::new_in("/tmp").unwrap();
        let root = temp_dir.path();

        std::fs::write(root.join("meta"), "1234").unwrap();

        temp_dir
    }

    #[fuchsia::test]
    async fn test_get_merkle_root() {
        let pkg_dir = create_pkg_dir();
        let query = serve_realm_query(
            vec![],
            HashMap::new(),
            HashMap::new(),
            HashMap::from([(("./my_foo".to_string(), fsys::OpenDirType::PackageDir), pkg_dir)]),
        );

        let moniker = AbsoluteMoniker::parse_str("/my_foo").unwrap();
        let merkle_root = get_merkle_root(&moniker, &query).await.unwrap();

        assert_eq!(merkle_root, "1234");
    }

    #[fuchsia::test]
    async fn test_get_instance() {
        let realm_query = serve_realm_query_instances(vec![fsys::Instance {
            moniker: Some("./my_foo".to_string()),
            url: Some("#meta/foo.cm".to_string()),
            instance_id: Some("1234567890".to_string()),
            resolved_info: Some(fsys::ResolvedInfo {
                resolved_url: Some("fuchsia-pkg://fuchsia.com/foo#meta/foo.cm".to_string()),
                execution_info: Some(fsys::ExecutionInfo {
                    start_reason: Some("Debugging Workflow".to_string()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        }]);

        let moniker = AbsoluteMoniker::parse_str("/my_foo").unwrap();
        let instance = get_instance(&moniker, &realm_query).await.unwrap();

        assert_eq!(instance.moniker, moniker);
        assert_eq!(instance.url, "#meta/foo.cm");
        assert_eq!(instance.instance_id.unwrap(), "1234567890");
        assert!(instance.resolved_info.is_some());

        let resolved = instance.resolved_info.unwrap();
        assert_eq!(resolved.resolved_url, "fuchsia-pkg://fuchsia.com/foo#meta/foo.cm");

        let execution_info = resolved.execution_info.unwrap();
        assert_eq!(execution_info.start_reason, "Debugging Workflow".to_string());
    }
}
