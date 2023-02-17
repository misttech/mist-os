// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    cm_rust::{ComponentDecl, FidlIntoNative},
    fidl_fuchsia_sys2 as fsys,
    moniker::{
        AbsoluteMoniker, AbsoluteMonikerBase, MonikerError, RelativeMoniker, RelativeMonikerBase,
    },
    thiserror::Error,
};

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
pub enum GetManifestError {
    #[error(transparent)]
    Fidl(#[from] fidl::Error),

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
pub struct Instance {
    /// Moniker of the component.
    pub moniker: AbsoluteMoniker,

    /// URL of the component.
    pub url: String,

    /// Unique identifier of component.
    pub instance_id: Option<String>,

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

        Ok(Self { moniker, url, instance_id: instance.instance_id, resolved_info })
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
    // Parse the runtime directory and add it into the State object
    let moniker_str = format!(".{}", moniker.to_string());
    match realm_query.get_manifest(&moniker_str).await? {
        Ok(decl) => Ok(decl.fidl_into_native()),
        Err(fsys::GetManifestError::InstanceNotFound) => {
            Err(GetManifestError::InstanceNotFound(moniker.clone()))
        }
        Err(fsys::GetManifestError::InstanceNotResolved) => {
            Err(GetManifestError::InstanceNotResolved(moniker.clone()))
        }
        Err(fsys::GetManifestError::BadMoniker) => {
            Err(GetManifestError::BadMoniker(moniker.clone()))
        }
        Err(_) => Err(GetManifestError::UnknownError),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::*;

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
                    ..fsys::ExecutionInfo::EMPTY
                }),
                ..fsys::ResolvedInfo::EMPTY
            }),
            ..fsys::Instance::EMPTY
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
}
