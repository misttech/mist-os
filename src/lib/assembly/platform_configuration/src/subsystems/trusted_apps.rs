// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use anyhow::{anyhow, Context};
use assembly_config_schema::{
    assembly_config::{CompiledComponentDefinition, CompiledPackageDefinition},
    product_config::TrustedApp as ProductTrustedApp,
};
use assembly_util::{BlobfsCompiledPackageDestination, CompiledPackageDestination};
use fuchsia_url::AbsoluteComponentUrl;
use std::io::Write;

fn create_name(name: &str) -> Result<cml::Name, anyhow::Error> {
    cml::Name::new(name).with_context(|| format!("Invalid name: {}", name))
}

pub(crate) struct TrustedAppsConfig;
impl DefineSubsystemConfiguration<Vec<ProductTrustedApp>> for TrustedAppsConfig {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        config: &Vec<ProductTrustedApp>,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        // If we don't have any trusted apps, we're done.
        if config.is_empty() {
            return Ok(());
        }

        let gendir = context.get_gendir()?;

        // Create a dictionary and expose it to the parent.
        let capabilities = vec![cml::Capability {
            dictionary: Some(create_name("trusted-app-capabilities")?),
            ..Default::default()
        }];
        let expose = vec![cml::Expose {
            dictionary: Some(create_name("trusted-app-capabilities")?.into()),
            ..cml::Expose::new_from(cml::ExposeFromRef::Self_.into())
        }];

        let mut children = vec![];
        let mut offer = vec![
            cml::Offer {
                protocol: Some(create_name("fuchsia.inspect.InspectSink")?.into()),
                ..cml::Offer::empty(cml::OfferFromRef::Parent.into(), cml::OfferToRef::All.into())
            },
            cml::Offer {
                protocol: Some(create_name("fuchsia.logger.LogSink")?.into()),
                ..cml::Offer::empty(cml::OfferFromRef::Parent.into(), cml::OfferToRef::All.into())
            },
        ];

        // Offer all of the protocols our children require to them.
        // Offer all of the protocols our children expose to the dictionary we
        // just created.
        for trusted_app in config {
            let component_url = AbsoluteComponentUrl::parse(&trusted_app.component_url)?;
            let component_name = create_name(
                component_url
                    .resource()
                    .split('/')
                    .last()
                    .ok_or_else(|| anyhow!("no resource name: {}", component_url.resource()))?
                    .split('.')
                    .next()
                    .ok_or_else(|| anyhow!("no component name: {}", component_url.resource()))?,
            )?;
            children.push(cml::Child {
                name: component_name.clone(),
                url: cm_types::Url::new(component_url.to_string())?,
                startup: cml::StartupMode::Lazy,
                on_terminate: None,
                environment: None,
            });

            for capability in &trusted_app.capabilities {
                // Expose the capabilities up from the component URL to the
                // dictionary we provide to the parent
                offer.push(cml::Offer {
                    protocol: Some(create_name(capability)?.into()),
                    availability: Some(cml::Availability::SameAsTarget),
                    ..cml::Offer::empty(
                        cml::OfferFromRef::Named(component_name.clone()).into(),
                        cml::OfferToRef::OwnDictionary(create_name("trusted-app-capabilities")?)
                            .into(),
                    )
                });
            }

            for guid in &trusted_app.guids {
                // Expose the guids from tee_manager to the component in question
                let guid_protocol_name = create_name(&format!("fuchsia.tee.Application.{}", guid))?;
                offer.push(cml::Offer {
                    protocol: Some(guid_protocol_name.into()),
                    ..cml::Offer::empty(
                        cml::OfferFromRef::Parent.into(),
                        cml::OfferToRef::Named(component_name.clone()).into(),
                    )
                });
            }

            for protocol in &trusted_app.additional_required_protocols {
                let protocol_name = create_name(protocol)?;
                offer.push(cml::Offer {
                    protocol: Some(protocol_name.into()),

                    // Most of these additional capabilities will come from
                    // tee_manager or factory_store_providers, and not all
                    // boards contain those components.
                    source_availability: Some(cml::SourceAvailability::Unknown),
                    ..cml::Offer::empty(
                        cml::OfferFromRef::Parent.into(),
                        cml::OfferToRef::Named(component_name.clone()).into(),
                    )
                });
            }
        }

        let cml = cml::Document {
            capabilities: Some(capabilities),
            expose: Some(expose),
            children: Some(children),
            offer: Some(offer),
            ..Default::default()
        };

        let cml_name = "trusted-apps.cml";
        let cml_path = gendir.join(cml_name);
        let mut cml_file = std::fs::File::create(&cml_path)?;
        cml_file.write_all(serde_json::to_string_pretty(&cml)?.as_bytes())?;

        let components = vec![CompiledComponentDefinition {
            component_name: "trusted-apps".into(),
            shards: vec![cml_path.into()],
        }];

        let destination =
            CompiledPackageDestination::Blob(BlobfsCompiledPackageDestination::TrustedApps);
        let def = CompiledPackageDefinition {
            name: destination.clone(),
            components,
            contents: Default::default(),
            includes: Default::default(),
            bootfs_package: Default::default(),
        };

        builder
            .compiled_package(destination.clone(), def)
            .with_context(|| format!("Inserting compiled package: {}", destination))?;
        builder.core_shard(&context.get_resource("trusted-apps.core_shard.cml"));

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subsystems::ConfigurationBuilderImpl;
    use assembly_file_relative_path::FileRelativePathBuf;

    #[test]
    // This test is a change detector, but we actually want to observe changes
    // in this type of code, which is dynamically generating CML for
    // components which might have security implications.
    fn test_define_configuration() {
        let context = ConfigurationContext::default_for_tests();
        let config = vec![ProductTrustedApp {
            component_url: "fuchsia-pkg://fuchsia.com/trusted-apps/test-app#meta/test-app.cm"
                .to_string(),
            guids: vec!["1234".to_string(), "5678".to_string()],
            additional_required_protocols: vec!["fuchsia.foo.bar".to_string()],
            capabilities: vec!["fuchsia.baz.bang".to_string()],
        }];

        let mut builder = ConfigurationBuilderImpl::default();
        TrustedAppsConfig::define_configuration(&context, &config, &mut builder)
            .expect("defining trusted_apps configuration");

        let completed_configuration = builder.build();
        let compiled_packages = completed_configuration.compiled_packages;
        assert_eq!(compiled_packages.len(), 1);

        let compiled_package = compiled_packages.values().next().unwrap();

        let shard: FileRelativePathBuf;
        if let CompiledPackageDefinition {
            name: CompiledPackageDestination::Blob(BlobfsCompiledPackageDestination::TrustedApps),
            components,
            contents,
            includes,
            bootfs_package: None,
        } = compiled_package
        {
            assert_eq!(components.len(), 1);
            let component = &components[0];
            assert_eq!(contents.len(), 0);
            assert_eq!(includes.len(), 0);

            assert_eq!(component.component_name, "trusted-apps");
            assert_eq!(component.shards.len(), 1);

            shard = component.shards[0].clone();
        } else {
            panic!("unexpected compiled package definition: {:#?}", compiled_package);
        }

        let contents = std::fs::read_to_string(shard).unwrap();
        let contents_json: serde_json::Value =
            serde_json::from_str(&contents).expect("parsing cml");

        let expected_json = serde_json::json!({"children": [
          {
            "name": "test-app",
            "url": "fuchsia-pkg://fuchsia.com/trusted-apps/test-app#meta/test-app.cm"
          }
        ],
        "capabilities": [
          {
            "dictionary": "trusted-app-capabilities"
          }
        ],
        "expose": [
          {
            "dictionary": "trusted-app-capabilities",
            "from": "self"
          }
        ],
        "offer": [
          {
            "protocol": "fuchsia.inspect.InspectSink",
            "from": "parent",
            "to": "all"
          },
          {
            "protocol": "fuchsia.logger.LogSink",
            "from": "parent",
            "to": "all"
          },
          {
            "protocol": "fuchsia.baz.bang",
            "from": "#test-app",
            "to": "self/trusted-app-capabilities",
            "availability": "same_as_target"
          },
          {
            "protocol": "fuchsia.tee.Application.1234",
            "from": "parent",
            "to": "#test-app"
          },
          {
            "protocol": "fuchsia.tee.Application.5678",
            "from": "parent",
            "to": "#test-app"
          },
          {
            "protocol": "fuchsia.foo.bar",
            "from": "parent",
            "to": "#test-app",
            "source_availability": "unknown"
          }
        ]});

        assert_eq!(
            expected_json, contents_json,
            "cml mismatch: Expected: \n\n{:#?}\n\nActual:n\n{:#?}",
            expected_json, contents_json,
        );
    }
}
