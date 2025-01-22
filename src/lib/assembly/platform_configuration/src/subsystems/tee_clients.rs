// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use anyhow::{anyhow, Context};
use assembly_config_schema::assembly_config::{
    CompiledComponentDefinition, CompiledPackageDefinition,
};
use assembly_config_schema::product_config::TeeClient as ProductTeeClient;
use assembly_constants::{BlobfsCompiledPackageDestination, CompiledPackageDestination, FileEntry};
use fuchsia_url::AbsoluteComponentUrl;
use std::io::Write;

#[derive(serde::Serialize)]
struct TeeManagerConfig {
    application_uuids: Vec<uuid::Uuid>,
}

fn create_name(name: &str) -> Result<cml::Name, anyhow::Error> {
    cml::Name::new(name).with_context(|| format!("Invalid name: {}", name))
}

pub(crate) struct TeeClientsConfig;
impl DefineSubsystemConfiguration<(&Vec<ProductTeeClient>, &Vec<uuid::Uuid>)> for TeeClientsConfig {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        product_config: &(&Vec<ProductTeeClient>, &Vec<uuid::Uuid>),
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        let (product_config, tee_trusted_app_guids) = product_config;
        match context.feature_set_level {
            // tee_manager and clients only exist in systems that have the core realm
            FeatureSupportLevel::Embeddable | FeatureSupportLevel::Bootstrap => return Ok(()),
            FeatureSupportLevel::Utility | FeatureSupportLevel::Standard => {}
        }

        // Configure the tee_manager based on whether the board provided GUIDs
        // to serve from it.
        if !tee_trusted_app_guids.is_empty() {
            create_tee_manager(tee_trusted_app_guids, context, builder)?;
        }

        // Hook up the clients of the tee_manager that the product has
        // registered to run under the session.
        if !product_config.is_empty() {
            create_tee_clients(product_config, context, builder)?;
        }

        Ok(())
    }
}

fn create_tee_manager(
    tee_trusted_app_guids: &&Vec<uuid::Uuid>,
    context: &ConfigurationContext<'_>,
    builder: &mut dyn ConfigurationBuilder,
) -> Result<(), anyhow::Error> {
    let tee_manager_config =
        TeeManagerConfig { application_uuids: (*tee_trusted_app_guids).clone() };
    let gendir = context.get_gendir()?;

    // Pull in the tee_manager platform bundle, which gives us the tee_manager
    // binary and various CML includes, as well as the tee_manager core shard.
    builder.platform_bundle("tee_manager");

    // Write tee_manager's config-data
    let tee_manager_config_path = gendir.join("tee_manager.config");
    let mut tee_manager_config_file = std::fs::File::create(&tee_manager_config_path)?;
    tee_manager_config_file
        .write_all(serde_json::to_string_pretty(&tee_manager_config)?.as_bytes())?;
    builder.package("tee_manager").config_data(FileEntry {
        source: tee_manager_config_path,
        destination: "tee_manager.config".into(),
    })?;

    // Create the tee_manager component definition itself.
    // tee_manager declares a capability and an expose for each GUID it is given
    // in the board configuration, as a protocol with a specific name format.
    let protocols: Vec<Option<cml::OneOrMany<cml::Name>>> = tee_trusted_app_guids
        .iter()
        .map(|guid| create_name(&format!("fuchsia.tee.Application.{}", guid)))
        .collect::<Result<Vec<cml::Name>, anyhow::Error>>()?
        .iter()
        .map(|name| Some(cml::OneOrMany::One(name.clone())))
        .collect();

    let capabilities = protocols
        .iter()
        .map(|protocol| cml::Capability { protocol: protocol.clone(), ..Default::default() })
        .collect();

    let expose = protocols
        .iter()
        .map(|protocol| cml::Expose {
            protocol: protocol.clone(),
            ..cml::Expose::new_from(cml::ExposeFromRef::Self_.into())
        })
        .collect();

    // Serialize the component.
    let cml = cml::Document {
        capabilities: Some(capabilities),
        expose: Some(expose),
        include: Some(vec!["tee_manager.base.cml".into()]),
        ..Default::default()
    };
    let cml_name = "tee_manager.cml";
    let cml_path = gendir.join(cml_name);
    let mut cml_file = std::fs::File::create(&cml_path)?;
    cml_file.write_all(serde_json::to_string_pretty(&cml)?.as_bytes())?;
    let components = vec![CompiledComponentDefinition {
        component_name: "tee_manager".into(),
        shards: vec![cml_path.into()],
    }];
    let destination =
        CompiledPackageDestination::Blob(BlobfsCompiledPackageDestination::TeeManager);
    let def = CompiledPackageDefinition {
        name: destination.clone(),
        components,

        // Contents and includes are set by the tee_manager AIB defined in
        // //bundles/assembly/BUILD.gn. This prevents us having to make
        // all of the contents and includes into assembly resources.
        contents: vec![],
        includes: vec![],
        bootfs_package: false,
    };
    builder
        .compiled_package(destination.clone(), def)
        .with_context(|| format!("Inserting compiled package: {}", destination))?;

    Ok(())
}

fn create_tee_clients(
    product_config: &&Vec<ProductTeeClient>,
    context: &ConfigurationContext<'_>,
    builder: &mut dyn ConfigurationBuilder,
) -> Result<(), anyhow::Error> {
    let gendir = context.get_gendir()?;

    let capabilities = vec![cml::Capability {
        dictionary: Some(create_name("tee-client-capabilities")?),
        ..Default::default()
    }];
    let expose = vec![cml::Expose {
        dictionary: Some(create_name("tee-client-capabilities")?.into()),
        ..cml::Expose::new_from(cml::ExposeFromRef::Self_.into())
    }];

    let mut children = vec![];

    let mut offer = vec![
        cml::Offer {
            dictionary: Some(create_name("diagnostics")?.into()),
            ..cml::Offer::empty(cml::OfferFromRef::Parent.into(), cml::OfferToRef::All.into())
        },
        cml::Offer {
            protocol: Some(create_name("fuchsia.tracing.provider.Registry")?.into()),
            availability: Some(cml::Availability::SameAsTarget),
            ..cml::Offer::empty(cml::OfferFromRef::Parent.into(), cml::OfferToRef::All.into())
        },
    ];

    for tee_client in *product_config {
        let component_url = AbsoluteComponentUrl::parse(&tee_client.component_url)?;
        let component_name = create_name(
            component_url
                .resource()
                .split('/')
                .next_back()
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

        for capability in &tee_client.capabilities {
            // Expose the capabilities up from the component URL to the
            // dictionary we provide to the parent
            offer.push(cml::Offer {
                protocol: Some(create_name(capability)?.into()),
                availability: Some(cml::Availability::SameAsTarget),
                ..cml::Offer::empty(
                    cml::OfferFromRef::Named(component_name.clone()).into(),
                    cml::OfferToRef::OwnDictionary(create_name("tee-client-capabilities")?).into(),
                )
            });
        }

        for guid in &tee_client.guids {
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

        for protocol in &tee_client.additional_required_protocols {
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

        if let Some(config_data) = &tee_client.config_data {
            // For each key in config data, add the file at the path of the value to config data
            let package_name = component_url.package_url().name();

            for (key, value) in config_data {
                builder
                    .package(package_name.as_ref())
                    .config_data(FileEntry { source: value.into(), destination: key.into() })
                    .context(format!(
                        "Adding config data file {} to package {}",
                        key, package_name
                    ))?;
            }

            // Route the config-data subdir named with the package-name to this component
            let directory_name = create_name("config-data")?;
            let subdir_name: cml::RelativePath = cm_types::RelativePath::new(package_name)?;
            offer.push(cml::Offer {
                directory: Some(directory_name.into()),
                subdir: Some(subdir_name),
                ..cml::Offer::empty(
                    cml::OfferFromRef::Parent.into(),
                    cml::OfferToRef::Named(component_name.clone()).into(),
                )
            })
        }

        if tee_client.additional_required_features.persistent_storage {
            offer.push(cml::Offer {
                storage: Some(create_name("data")?.into()),
                ..cml::Offer::empty(
                    cml::OfferFromRef::Parent.into(),
                    cml::OfferToRef::Named(component_name.clone()).into(),
                )
            })
        }

        if tee_client.additional_required_features.tmp_storage {
            offer.push(cml::Offer {
                storage: Some(create_name("tmp")?.into()),
                ..cml::Offer::empty(
                    cml::OfferFromRef::Parent.into(),
                    cml::OfferToRef::Named(component_name.clone()).into(),
                )
            })
        }

        if tee_client.additional_required_features.securemem {
            offer.push(cml::Offer {
                directory: Some(create_name("dev-securemem")?.into()),
                rights: Some(cml::Rights(vec![cml::Right::ReadAlias])),
                ..cml::Offer::empty(
                    cml::OfferFromRef::Parent.into(),
                    cml::OfferToRef::Named(component_name.clone()).into(),
                )
            })
        }
    }

    let cml = cml::Document {
        capabilities: Some(capabilities),
        expose: Some(expose),
        children: Some(children),
        offer: Some(offer),
        ..Default::default()
    };

    let cml_name = "tee-clients.cml";
    let cml_path = gendir.join(cml_name);
    let mut cml_file = std::fs::File::create(&cml_path)?;

    cml_file.write_all(serde_json::to_string_pretty(&cml)?.as_bytes())?;
    let components = vec![CompiledComponentDefinition {
        component_name: "tee-clients".into(),
        shards: vec![cml_path.into()],
    }];

    let destination =
        CompiledPackageDestination::Blob(BlobfsCompiledPackageDestination::TeeClients);
    let def = CompiledPackageDefinition {
        name: destination.clone(),
        components,
        contents: Default::default(),
        includes: Default::default(),
        bootfs_package: false,
    };

    builder
        .compiled_package(destination.clone(), def)
        .with_context(|| format!("Inserting compiled package: {}", destination))?;
    builder.core_shard(&context.get_resource("tee-clients.core_shard.cml"));

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subsystems::ConfigurationBuilderImpl;
    use assembly_config_schema::product_config::TeeClientFeatures;
    use assembly_file_relative_path::FileRelativePathBuf;
    use std::collections::BTreeMap;

    #[test]
    // This test is a change detector, but we actually want to observe changes
    // in this type of code, which is dynamically generating CML for
    // components which might have security implications.
    fn test_tee_clients() {
        let (context, tee_client_config, tee_trusted_app_guids, mut builder) = setup_test();

        TeeClientsConfig::define_configuration(
            &context,
            &(&tee_client_config, &tee_trusted_app_guids),
            &mut builder,
        )
        .expect("defining tee_clients configuration");

        let completed_configuration = builder.build();
        let compiled_packages = completed_configuration.compiled_packages;
        assert_eq!(compiled_packages.len(), 2);

        let compiled_package = compiled_packages
            .get(&CompiledPackageDestination::Blob(BlobfsCompiledPackageDestination::TeeClients))
            .unwrap();

        // Verify that we created tee-clients correctly
        let shard: FileRelativePathBuf;
        if let CompiledPackageDefinition {
            name: CompiledPackageDestination::Blob(BlobfsCompiledPackageDestination::TeeClients),
            components,
            contents,
            includes,
            bootfs_package: false,
        } = compiled_package
        {
            assert_eq!(components.len(), 1);
            let component = &components[0];
            assert_eq!(contents.len(), 0);
            assert_eq!(includes.len(), 0);

            assert_eq!(component.component_name, "tee-clients");
            assert_eq!(component.shards.len(), 1);

            shard = component.shards[0].clone();
        } else {
            panic!("unexpected compiled package definition: {:#?}", compiled_package);
        }

        let contents = std::fs::read_to_string(shard.clone()).unwrap();
        eprintln!("contents: {}", contents);
        eprintln!("path: {}", shard);
        let contents_json: serde_json::Value =
            serde_json::from_str(&contents).expect("parsing cml");

        let expected_json = serde_json::json!({"children": [
          {
            "name": "test-app",
            "url": "fuchsia-pkg://fuchsia.com/tee-clients/test-app#meta/test-app.cm"
          }
        ],
        "capabilities": [
          {
            "dictionary": "tee-client-capabilities"
          }
        ],
        "expose": [
          {
            "dictionary": "tee-client-capabilities",
            "from": "self"
          }
        ],
        "offer": [
          {
            "dictionary": "diagnostics",
            "from": "parent",
            "to": "all"
          },
          {
            "protocol": "fuchsia.tracing.provider.Registry",
            "from": "parent",
            "to": "all",
            "availability": "same_as_target",
          },
          {
            "protocol": "fuchsia.baz.bang",
            "from": "#test-app",
            "to": "self/tee-client-capabilities",
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
          },
          {
            "directory": "config-data",
            "from": "parent",
            "to": "#test-app",
            "subdir": "tee-clients",
          },
          {
            "storage": "data",
            "from": "parent",
            "to": "#test-app",
          },
          {
            "storage": "tmp",
            "from": "parent",
            "to": "#test-app",
          },
          {
            "directory": "dev-securemem",
            "from": "parent",
            "rights": ["r*"],
            "to": "#test-app",
          }
        ]});

        assert_eq!(
            expected_json, contents_json,
            "cml mismatch: Expected: \n\n{:#?}\n\nActual:\n\n{:#?}",
            expected_json, contents_json,
        );
    }

    #[test]
    fn test_tee_manager() {
        let (context, tee_client_config, tee_trusted_app_guids, mut builder) = setup_test();
        TeeClientsConfig::define_configuration(
            &context,
            &(&tee_client_config, &tee_trusted_app_guids),
            &mut builder,
        )
        .expect("defining tee_clients configuration");

        let completed_configuration = builder.build();
        let compiled_packages = completed_configuration.compiled_packages;
        assert_eq!(compiled_packages.len(), 2);

        // Verify that we hooked up tee_manager correctly, including
        // - config data
        // - core shard
        // - routing from tee_manager to other components
        let tee_manager_config_data = &completed_configuration
            .package_configs
            .get("tee_manager")
            .expect("getting config data for tee_manager")
            .config_data;
        let tee_manager_config_path = &tee_manager_config_data
            .map
            .entries
            .get(&String::from("tee_manager.config"))
            .expect("getting tee manager config")
            .source;
        let tee_manager_config =
            std::fs::read_to_string(tee_manager_config_path).expect("opening tee manager config");
        let tee_manager_config_json: serde_json::Value =
            serde_json::from_str(&tee_manager_config).expect("parsing config");

        let expected_json = serde_json::json!({"application_uuids": [
            "9105f952-86db-4808-bc0e-8a4172e11843",
            "826a0526-cd9b-4a61-a96e-1fd5b53061a3",
            ]
        });

        assert_eq!(
            expected_json, tee_manager_config_json,
            "config mismatch: Expected: \n\n{:#?}\n\nActual:\n\n{:#?}",
            expected_json, tee_manager_config_json,
        );

        assert!(completed_configuration.bundles.contains("tee_manager"));

        let compiled_package = compiled_packages
            .get(&CompiledPackageDestination::Blob(BlobfsCompiledPackageDestination::TeeManager))
            .unwrap();

        // Verify that we created tee_manager correctly
        let shard: FileRelativePathBuf;
        if let CompiledPackageDefinition {
            name: CompiledPackageDestination::Blob(BlobfsCompiledPackageDestination::TeeManager),
            components,
            contents,
            includes,
            bootfs_package: false,
        } = compiled_package
        {
            assert_eq!(components.len(), 1);
            let component = &components[0];
            assert_eq!(contents.len(), 0); // contents come from the tee_manager AIB.
            assert_eq!(includes.len(), 0);

            assert_eq!(component.component_name, "tee_manager");
            assert_eq!(component.shards.len(), 1);

            shard = component.shards[0].clone();
        } else {
            panic!("unexpected compiled package definition: {:#?}", compiled_package);
        }

        let contents = std::fs::read_to_string(shard).unwrap();
        let contents_json: serde_json::Value =
            serde_json::from_str(&contents).expect("parsing cml");

        let expected_json = serde_json::json!({
            "include": ["tee_manager.base.cml"],
            "capabilities": [
                {
                    "protocol": "fuchsia.tee.Application.9105f952-86db-4808-bc0e-8a4172e11843",
                },
                {
                    "protocol" : "fuchsia.tee.Application.826a0526-cd9b-4a61-a96e-1fd5b53061a3",
                }
            ],
            "expose": [
                {
                    "from": "self",
                    "protocol" :"fuchsia.tee.Application.9105f952-86db-4808-bc0e-8a4172e11843",
                },
                {
                    "from": "self",
                    "protocol" : "fuchsia.tee.Application.826a0526-cd9b-4a61-a96e-1fd5b53061a3",
                }
            ],
        });

        assert_eq!(
            expected_json, contents_json,
            "cml mismatch: Expected: \n\n{:#?}\n\nActual:\n\n{:#?}",
            expected_json, contents_json,
        );
    }

    fn setup_test() -> (
        ConfigurationContext<'static>,
        Vec<ProductTeeClient>,
        Vec<uuid::Uuid>,
        ConfigurationBuilderImpl,
    ) {
        let context = ConfigurationContext::default_for_tests();

        let tee_client_config = vec![ProductTeeClient {
            component_url: "fuchsia-pkg://fuchsia.com/tee-clients/test-app#meta/test-app.cm"
                .to_string(),
            guids: vec!["1234".to_string(), "5678".to_string()],
            additional_required_protocols: vec!["fuchsia.foo.bar".to_string()],
            capabilities: vec!["fuchsia.baz.bang".to_string()],
            config_data: Some(BTreeMap::from([
                ("foo".to_string(), "bar".to_string()),
                ("baz".to_string(), "qux".to_string()),
            ])),
            additional_required_features: TeeClientFeatures {
                tmp_storage: true,
                persistent_storage: true,
                securemem: true,
            },
        }];

        let tee_trusted_app_guids = vec![
            uuid::Uuid::parse_str("9105f952-86db-4808-bc0e-8a4172e11843").unwrap(),
            uuid::Uuid::parse_str("826a0526-cd9b-4a61-a96e-1fd5b53061a3").unwrap(),
        ];

        let builder = ConfigurationBuilderImpl::default();
        (context, tee_client_config, tee_trusted_app_guids, builder)
    }
}
