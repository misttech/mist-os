// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use anyhow::{Context, Result};
use assembly_config_schema::product_config::TrustedApp as ProductTrustedApp;
use assembly_constants::{BootfsPackageDestination, PackageSetDestination};
use assembly_images_config::FilesystemImageMode;
use fuchsia_tee_manager_config::TAConfig;
use fuchsia_url::boot_url::BootUrl;
use fuchsia_url::AbsoluteComponentUrl;

pub(crate) struct TrustedAppsSubsystem;
impl DefineSubsystemConfiguration<(&Vec<ProductTrustedApp>, FilesystemImageMode)>
    for TrustedAppsSubsystem
{
    fn define_configuration(
        _context: &ConfigurationContext<'_>,
        config: &(&Vec<ProductTrustedApp>, FilesystemImageMode),
        builder: &mut dyn ConfigurationBuilder,
    ) -> Result<()> {
        let (trusted_apps, image_mode) = config;
        if trusted_apps.is_empty() {
            return Ok(());
        }

        builder.platform_bundle("trusted_execution_environment");

        // Create a domain config package for all the TA configs.
        let dir = builder
            .add_domain_config(PackageSetDestination::Boot(
                BootfsPackageDestination::TaManagerConfig,
            ))
            .directory("config");

        // Add the configs for all the TAs.
        for c in trusted_apps.iter() {
            let url = if *image_mode == FilesystemImageMode::NoImage {
                let url = AbsoluteComponentUrl::parse(&c.component_url)
                    .with_context(|| format!("Parsing: {}", &c.component_url))?;
                let url = BootUrl::try_from(&url)
                    .with_context(|| format!("Convert to a boot url: {}", &url))?;
                url.to_string()
            } else {
                c.component_url.clone()
            };
            let ta_config = TAConfig::new(url);
            let ta_config = serde_json::to_string(&ta_config)
                .with_context(|| format!("Failed to serialize the ta config: {}", c.guid))?;
            dir.entry_from_contents(&format!("{}.json", c.guid), &ta_config)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subsystems::ConfigurationBuilderImpl;
    use crate::FileOrContents;

    #[test]
    fn test_convert_component_urls_to_boot_urls() {
        let context = ConfigurationContext {
            feature_set_level: &FeatureSupportLevel::Standard,
            build_type: &BuildType::Eng,
            board_info: &Default::default(),
            gendir: Default::default(),
            resource_dir: Default::default(),
            developer_only_options: Default::default(),
        };
        let mut builder: ConfigurationBuilderImpl = Default::default();
        let trusted_apps = vec![
            ProductTrustedApp {
                component_url: "fuchsia-pkg://fuchsia.com/pkg1#resource1.txt".into(),
                guid: "abcdef".into(),
            },
            ProductTrustedApp {
                component_url: "fuchsia-pkg://fuchsia.com/pkg2#resource2.txt".into(),
                guid: "123456".into(),
            },
        ];
        let image_mode = FilesystemImageMode::NoImage;
        let result = TrustedAppsSubsystem::define_configuration(
            &context,
            &(&trusted_apps, image_mode),
            &mut builder,
        );
        assert!(result.is_ok());
        let config = builder.build();
        let domain_config = config
            .domain_configs
            .get(&PackageSetDestination::Boot(BootfsPackageDestination::TaManagerConfig))
            .unwrap();
        let directory = domain_config.directories.get("config").unwrap();
        let entries: Vec<FileOrContents> =
            directory.entries.iter().map(|(_, e)| e.clone()).collect();
        let expected_configs = [
            TAConfig::new("fuchsia-boot:///pkg2#resource2.txt".into()),
            TAConfig::new("fuchsia-boot:///pkg1#resource1.txt".into()),
        ];
        let expected_configs: Vec<FileOrContents> = expected_configs
            .iter()
            .map(|c| FileOrContents::Contents(serde_json::to_string(c).unwrap()))
            .collect();
        assert_eq!(entries, expected_configs);
    }
}
