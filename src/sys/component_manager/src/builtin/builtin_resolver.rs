// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ::routing::resolving;
use async_trait::async_trait;
use fuchsia_url::builtin_url::BuiltinUrl;
use include_bytes_from_working_dir::include_bytes_from_working_dir_env;
use routing::resolving::{ComponentAddress, ResolvedComponent, ResolverError};
use thiserror::Error;

use crate::model::resolver::Resolver;

pub static SCHEME: &str = "fuchsia-builtin";

/// The builtin resolver resolves components defined inside component_manager.
///
/// It accepts a URL scheme of the form `fuchsia-builtin://#elf_runner.cm`.
///
/// Builtin component declarations are used to bootstrap the ELF runner.
/// They are never packaged.
///
/// Only absolute URLs are supported.
#[derive(Debug)]
pub struct BuiltinResolver {}

#[async_trait]
impl Resolver for BuiltinResolver {
    async fn resolve(
        &self,
        component_address: &ComponentAddress,
    ) -> Result<ResolvedComponent, ResolverError> {
        if component_address.is_relative_path() {
            return Err(ResolverError::UnexpectedRelativePath(component_address.url().to_string()));
        }
        let url = BuiltinUrl::parse(component_address.url())
            .map_err(|e| ResolverError::malformed_url(e))?;
        let Some(resource) = url.resource() else {
            return Err(ResolverError::manifest_not_found(ManifestNotFoundError(url)));
        };
        let cm = match resource {
            "elf_runner.cm" => Ok(ELF_RUNNER_CM),
            _ => Err(ResolverError::manifest_not_found(ManifestNotFoundError(url.clone()))),
        }?;
        let decl = resolving::read_and_validate_manifest_bytes(cm)?;

        // Unpackaged components built into component_manager are assigned the
        // platform abi revision.
        let abi_revision = version_history_data::HISTORY.get_abi_revision_for_platform_components();

        Ok(ResolvedComponent {
            resolved_url: url.to_string(),
            context_to_resolve_children: None,
            decl,
            package: None,
            config_values: None,
            abi_revision: Some(abi_revision),
        })
    }
}

/// A compiled `.cm` binary blob corresponding to the ELF runner component declaration.
static ELF_RUNNER_CM: &'static [u8] = include_bytes_from_working_dir_env!("ELF_RUNNER_CM_PATH");

#[derive(Error, Debug, Clone)]
#[error("{0} does not reference a known manifest. Try fuchsia-builtin://#elf_runner.cm")]
struct ManifestNotFoundError(pub BuiltinUrl);

#[cfg(test)]
mod tests {
    use super::*;

    #[fuchsia::test]
    fn elf_runner_cm_smoke_test() {
        let decl = resolving::read_and_validate_manifest_bytes(ELF_RUNNER_CM).unwrap();
        let program = decl.program.unwrap();
        assert_eq!(program.runner.unwrap().as_str(), "builtin_elf_runner");
    }
}
