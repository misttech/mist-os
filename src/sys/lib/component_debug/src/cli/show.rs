// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        query::get_single_instance_from_query,
        realm::{
            get_config_fields, get_merkle_root, get_outgoing_capabilities,
            get_resolved_declaration, get_runtime, ConfigField, ExecutionInfo, ResolvedInfo,
            Runtime,
        },
    },
    ansi_term::Colour,
    anyhow::Result,
    cm_rust::ExposeDeclCommon,
    fidl_fuchsia_sys2 as fsys,
    moniker::Moniker,
    prettytable::{cell, format::FormatBuilder, row, Table},
};

#[cfg(feature = "serde")]
use serde::Serialize;

#[cfg_attr(feature = "serde", derive(Serialize))]
pub struct ShowCmdInstance {
    pub moniker: Moniker,
    pub url: String,
    pub environment: Option<String>,
    pub instance_id: Option<String>,
    pub resolved: Option<ShowCmdResolvedInfo>,
}

#[cfg_attr(feature = "serde", derive(Serialize))]
pub struct ShowCmdResolvedInfo {
    pub resolved_url: String,
    pub merkle_root: Option<String>,
    pub incoming_capabilities: Vec<String>,
    pub exposed_capabilities: Vec<String>,
    pub config: Option<Vec<ConfigField>>,
    pub started: Option<ShowCmdExecutionInfo>,
    pub collections: Vec<String>,
}

#[cfg_attr(feature = "serde", derive(Serialize))]
pub struct ShowCmdExecutionInfo {
    pub runtime: Runtime,
    pub outgoing_capabilities: Vec<String>,
    pub start_reason: String,
}

pub async fn show_cmd_print<W: std::io::Write>(
    query: String,
    realm_query: fsys::RealmQueryProxy,
    mut writer: W,
) -> Result<()> {
    let instance = get_instance_by_query(query, realm_query).await?;
    let table = create_table(instance);
    table.print(&mut writer)?;
    writeln!(&mut writer, "")?;

    Ok(())
}

pub async fn show_cmd_serialized(
    query: String,
    realm_query: fsys::RealmQueryProxy,
) -> Result<ShowCmdInstance> {
    let instance = get_instance_by_query(query, realm_query).await?;
    Ok(instance)
}

async fn get_instance_by_query(
    query: String,
    realm_query: fsys::RealmQueryProxy,
) -> Result<ShowCmdInstance> {
    let instance = get_single_instance_from_query(&query, &realm_query).await?;

    let resolved_info = match instance.resolved_info {
        Some(ResolvedInfo { execution_info, resolved_url }) => {
            // Get the manifest
            let manifest = get_resolved_declaration(&instance.moniker, &realm_query).await?;
            let structured_config = get_config_fields(&instance.moniker, &realm_query).await?;
            let merkle_root = get_merkle_root(&instance.moniker, &realm_query).await.ok();
            let incoming_capabilities =
                manifest.uses.into_iter().filter_map(|u| u.path().map(|n| n.to_string())).collect();
            let exposed_capabilities =
                manifest.exposes.into_iter().map(|e| e.target_name().to_string()).collect();

            let execution_info = match execution_info {
                Some(ExecutionInfo { start_reason }) => {
                    let runtime = get_runtime(&instance.moniker, &realm_query)
                        .await
                        .unwrap_or(Runtime::Unknown);
                    let outgoing_capabilities =
                        get_outgoing_capabilities(&instance.moniker, &realm_query)
                            .await
                            .unwrap_or(vec![]);
                    Some(ShowCmdExecutionInfo { start_reason, runtime, outgoing_capabilities })
                }
                None => None,
            };

            let collections =
                manifest.collections.into_iter().map(|c| c.name.to_string()).collect();

            Some(ShowCmdResolvedInfo {
                resolved_url,
                incoming_capabilities,
                exposed_capabilities,
                merkle_root,
                config: structured_config,
                started: execution_info,
                collections,
            })
        }
        None => None,
    };

    Ok(ShowCmdInstance {
        moniker: instance.moniker,
        url: instance.url,
        environment: instance.environment,
        instance_id: instance.instance_id,
        resolved: resolved_info,
    })
}

fn create_table(instance: ShowCmdInstance) -> Table {
    let mut table = Table::new();
    table.set_format(FormatBuilder::new().padding(2, 0).build());

    table.add_row(row!(r->"Moniker:", instance.moniker));
    table.add_row(row!(r->"URL:", instance.url));
    table.add_row(
        row!(r->"Environment:", instance.environment.unwrap_or_else(|| "N/A".to_string())),
    );

    if let Some(instance_id) = instance.instance_id {
        table.add_row(row!(r->"Instance ID:", instance_id));
    } else {
        table.add_row(row!(r->"Instance ID:", "None"));
    }

    add_resolved_info_to_table(&mut table, instance.resolved);

    table
}

fn add_resolved_info_to_table(table: &mut Table, resolved: Option<ShowCmdResolvedInfo>) {
    if let Some(resolved) = resolved {
        table.add_row(row!(r->"Component State:", Colour::Green.paint("Resolved")));
        table.add_row(row!(r->"Resolved URL:", resolved.resolved_url));

        let namespace_capabilities = resolved.incoming_capabilities.join("\n");
        table.add_row(row!(r->"Namespace Capabilities:", namespace_capabilities));

        let exposed_capabilities = resolved.exposed_capabilities.join("\n");
        table.add_row(row!(r->"Exposed Capabilities:", exposed_capabilities));

        if let Some(merkle_root) = &resolved.merkle_root {
            table.add_row(row!(r->"Merkle root:", merkle_root));
        } else {
            table.add_row(row!(r->"Merkle root:", "Unknown"));
        }

        if let Some(config) = &resolved.config {
            if !config.is_empty() {
                let mut config_table = Table::new();
                let format = FormatBuilder::new().padding(0, 0).build();
                config_table.set_format(format);

                for field in config {
                    config_table.add_row(row!(field.key, " -> ", field.value));
                }

                table.add_row(row!(r->"Configuration:", config_table));
            }
        }

        if !resolved.collections.is_empty() {
            table.add_row(row!(r->"Collections:", resolved.collections.join("\n")));
        }

        add_execution_info_to_table(table, resolved.started)
    } else {
        table.add_row(row!(r->"Component State:", Colour::Red.paint("Unresolved")));
    }
}

fn add_execution_info_to_table(table: &mut Table, exec: Option<ShowCmdExecutionInfo>) {
    if let Some(exec) = exec {
        table.add_row(row!(r->"Execution State:", Colour::Green.paint("Running")));
        table.add_row(row!(r->"Start reason:", exec.start_reason));

        let outgoing_capabilities = exec.outgoing_capabilities.join("\n");
        table.add_row(row!(r->"Outgoing Capabilities:", outgoing_capabilities));

        match exec.runtime {
            Runtime::Elf {
                job_id,
                process_id,
                process_start_time,
                process_start_time_utc_estimate,
            } => {
                table.add_row(row!(r->"Runtime:", "ELF"));
                if let Some(utc_estimate) = process_start_time_utc_estimate {
                    table.add_row(row!(r->"Running since:", utc_estimate));
                } else if let Some(ticks) = process_start_time {
                    table.add_row(row!(r->"Running since:", format!("{} ticks", ticks)));
                }

                table.add_row(row!(r->"Job ID:", job_id));

                if let Some(process_id) = process_id {
                    table.add_row(row!(r->"Process ID:", process_id));
                }
            }
            Runtime::Unknown => {
                table.add_row(row!(r->"Runtime:", "Unknown"));
            }
        }
    } else {
        table.add_row(row!(r->"Execution State:", Colour::Red.paint("Stopped")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::*;
    use fidl_fuchsia_component_decl as fdecl;
    use moniker::{Moniker, MonikerBase};
    use std::collections::HashMap;
    use std::fs;
    use tempfile::TempDir;

    pub fn create_pkg_dir() -> TempDir {
        let temp_dir = TempDir::new_in("/tmp").unwrap();
        let root = temp_dir.path();

        fs::write(root.join("meta"), "1234").unwrap();

        temp_dir
    }

    pub fn create_out_dir() -> TempDir {
        let temp_dir = TempDir::new_in("/tmp").unwrap();
        let root = temp_dir.path();

        fs::create_dir(root.join("diagnostics")).unwrap();

        temp_dir
    }

    pub fn create_runtime_dir() -> TempDir {
        let temp_dir = TempDir::new_in("/tmp").unwrap();
        let root = temp_dir.path();

        fs::create_dir_all(root.join("elf")).unwrap();
        fs::write(root.join("elf/job_id"), "1234").unwrap();
        fs::write(root.join("elf/process_id"), "2345").unwrap();
        fs::write(root.join("elf/process_start_time"), "3456").unwrap();
        fs::write(root.join("elf/process_start_time_utc_estimate"), "abcd").unwrap();

        temp_dir
    }

    fn create_query() -> fsys::RealmQueryProxy {
        // Serve RealmQuery for CML components.
        let out_dir = create_out_dir();
        let pkg_dir = create_pkg_dir();
        let runtime_dir = create_runtime_dir();

        let query = serve_realm_query(
            vec![
                fsys::Instance {
                    moniker: Some("./my_foo".to_string()),
                    url: Some("fuchsia-pkg://fuchsia.com/foo#meta/foo.cm".to_string()),
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
                },
                fsys::Instance {
                    moniker: Some("./core/appmgr".to_string()),
                    url: Some("fuchsia-pkg://fuchsia.com/appmgr#meta/appmgr.cm".to_string()),
                    instance_id: None,
                    resolved_info: Some(fsys::ResolvedInfo {
                        resolved_url: Some(
                            "fuchsia-pkg://fuchsia.com/appmgr#meta/appmgr.cm".to_string(),
                        ),
                        execution_info: Some(fsys::ExecutionInfo {
                            start_reason: Some("Debugging Workflow".to_string()),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            ],
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
                    collections: Some(vec![fdecl::Collection {
                        name: Some("my-collection".to_string()),
                        durability: Some(fdecl::Durability::Transient),
                        ..Default::default()
                    }]),
                    ..Default::default()
                },
            )]),
            HashMap::from([(
                "./my_foo".to_string(),
                fdecl::ResolvedConfig {
                    fields: vec![fdecl::ResolvedConfigField {
                        key: "foo".to_string(),
                        value: fdecl::ConfigValue::Single(fdecl::ConfigSingleValue::Bool(false)),
                    }],
                    checksum: fdecl::ConfigChecksum::Sha256([0; 32]),
                },
            )]),
            HashMap::from([
                (("./my_foo".to_string(), fsys::OpenDirType::RuntimeDir), runtime_dir),
                (("./my_foo".to_string(), fsys::OpenDirType::PackageDir), pkg_dir),
                (("./my_foo".to_string(), fsys::OpenDirType::OutgoingDir), out_dir),
            ]),
        );
        query
    }

    #[fuchsia::test]
    async fn basic_cml() {
        let query = create_query();

        let instance = get_instance_by_query("foo.cm".to_string(), query).await.unwrap();

        assert_eq!(instance.moniker, Moniker::parse_str("/my_foo").unwrap());
        assert_eq!(instance.url, "fuchsia-pkg://fuchsia.com/foo#meta/foo.cm");
        assert_eq!(instance.instance_id.unwrap(), "1234567890");
        assert!(instance.resolved.is_some());

        let resolved = instance.resolved.unwrap();
        assert_eq!(resolved.incoming_capabilities.len(), 1);
        assert_eq!(resolved.incoming_capabilities[0], "/svc/fuchsia.foo.bar");

        assert_eq!(resolved.exposed_capabilities.len(), 1);
        assert_eq!(resolved.exposed_capabilities[0], "fuchsia.bar.baz");

        assert_eq!(resolved.merkle_root.unwrap(), "1234");

        let config = resolved.config.unwrap();
        assert_eq!(
            config,
            vec![ConfigField { key: "foo".to_string(), value: "Bool(false)".to_string() }]
        );

        assert_eq!(resolved.collections, vec!["my-collection"]);

        let started = resolved.started.unwrap();
        assert_eq!(started.outgoing_capabilities, vec!["diagnostics".to_string()]);
        assert_eq!(started.start_reason, "Debugging Workflow".to_string());

        match started.runtime {
            Runtime::Elf {
                job_id,
                process_id,
                process_start_time,
                process_start_time_utc_estimate,
            } => {
                assert_eq!(job_id, 1234);
                assert_eq!(process_id, Some(2345));
                assert_eq!(process_start_time, Some(3456));
                assert_eq!(process_start_time_utc_estimate, Some("abcd".to_string()));
            }
            _ => panic!("unexpected runtime"),
        }
    }

    #[fuchsia::test]
    async fn find_by_moniker() {
        let query = create_query();

        let instance = get_instance_by_query("my_foo".to_string(), query).await.unwrap();

        assert_eq!(instance.moniker, Moniker::parse_str("/my_foo").unwrap());
        assert_eq!(instance.url, "fuchsia-pkg://fuchsia.com/foo#meta/foo.cm");
        assert_eq!(instance.instance_id.unwrap(), "1234567890");
    }

    #[fuchsia::test]
    async fn find_by_instance_id() {
        let query = create_query();

        let instance = get_instance_by_query("1234567".to_string(), query).await.unwrap();

        assert_eq!(instance.moniker, Moniker::parse_str("/my_foo").unwrap());
        assert_eq!(instance.url, "fuchsia-pkg://fuchsia.com/foo#meta/foo.cm");
        assert_eq!(instance.instance_id.unwrap(), "1234567890");
    }
}
