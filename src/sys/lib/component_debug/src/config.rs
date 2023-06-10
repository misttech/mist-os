// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::realm::resolve_declaration;
use cm_rust::NativeIntoFidl;
use config_value_file::field::config_value_from_json_value;
use fidl_fuchsia_component_decl as fdecl;
use fidl_fuchsia_sys2 as fsys;
use moniker::{AbsoluteMoniker, AbsoluteMonikerBase, ChildMonikerBase};

pub async fn resolve_raw_config_overrides(
    realm_query: &fsys::RealmQueryProxy,
    moniker: &AbsoluteMoniker,
    url: &str,
    raw_overrides: &[RawConfigOverride],
) -> Result<Vec<fdecl::ConfigOverride>, ConfigResolveError> {
    // Don't worry about any of the below failure modes if there aren't actually any overrides.
    if raw_overrides.is_empty() {
        return Ok(vec![]);
    }

    let parent = moniker.parent().ok_or_else(|| ConfigResolveError::BadMoniker(moniker.clone()))?;
    let leaf = moniker.leaf().ok_or_else(|| ConfigResolveError::BadMoniker(moniker.clone()))?;
    let collection =
        leaf.collection().ok_or_else(|| ConfigResolveError::BadMoniker(moniker.clone()))?;
    let child_location = fsys::ChildLocation::Collection(collection.to_string());
    let manifest = resolve_declaration(realm_query, &parent, &child_location, url).await?;
    let config = manifest.config.as_ref().ok_or(ConfigResolveError::MissingConfigSchema)?;

    let mut resolved_overrides = vec![];
    for raw_override in raw_overrides {
        let config_field =
            config.fields.iter().find(|f| f.key == raw_override.key).ok_or_else(|| {
                ConfigResolveError::MissingConfigField { name: raw_override.key.clone() }
            })?;

        let resolved_value = config_value_from_json_value(&raw_override.value, &config_field.type_)
            .map_err(ConfigResolveError::FieldTypeError)?;
        resolved_overrides.push(fdecl::ConfigOverride {
            key: Some(raw_override.key.clone()),
            value: Some(resolved_value.native_into_fidl()),
            ..fdecl::ConfigOverride::default()
        });
    }

    Ok(resolved_overrides)
}

#[derive(Debug, PartialEq)]
pub struct RawConfigOverride {
    key: String,
    value: serde_json::Value,
}

impl std::str::FromStr for RawConfigOverride {
    type Err = ConfigParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (key, value) = s.split_once("=").ok_or(ConfigParseError::MissingEqualsSeparator)?;
        Ok(Self {
            key: key.to_owned(),
            value: serde_json::from_str(&value).map_err(ConfigParseError::InvalidJsonValue)?,
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigResolveError {
    #[error("`{_0}` does not reference a dynamic instance.")]
    BadMoniker(AbsoluteMoniker),

    #[error("Failed to get component manifest: {_0:?}")]
    FailedToGetManifest(
        #[source]
        #[from]
        crate::realm::GetDeclarationError,
    ),

    #[error("Provided component URL points to a manifest without a config schema.")]
    MissingConfigSchema,

    #[error("Component does not have a config field named `{name}`.")]
    MissingConfigField { name: String },

    #[error("Couldn't resolve provided config value to the declared type in component manifest.")]
    FieldTypeError(#[source] config_value_file::field::FieldError),
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigParseError {
    #[error("Config override did not have a `=` to delimit key and value strings.")]
    MissingEqualsSeparator,

    #[error("Unable to parse provided value as JSON.")]
    InvalidJsonValue(#[source] serde_json::Error),
}
