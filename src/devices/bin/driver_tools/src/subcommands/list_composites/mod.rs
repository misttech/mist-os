// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod args;

use {
    crate::common::write_node_properties,
    anyhow::{Context, Result},
    args::ListCompositesCommand,
    fidl_fuchsia_driver_development as fdd, fidl_fuchsia_driver_framework as fdf,
    fidl_fuchsia_driver_legacy as fdl,
    std::io::Write,
};

pub async fn list_composites(
    cmd: ListCompositesCommand,
    writer: &mut dyn Write,
    proxy: fdd::ManagerProxy,
) -> Result<()> {
    let (iterator, iterator_server) =
        fidl::endpoints::create_proxy::<fdd::CompositeInfoIteratorMarker>()?;
    proxy.get_composite_info(iterator_server).context("GetCompositeInfo() failed")?;

    loop {
        let composite_list =
            iterator.get_next().await.context("CompositeInfoIterator GetNext() failed")?;

        if composite_list.is_empty() {
            break;
        }

        for composite_node in composite_list {
            match composite_node.composite {
                Some(fdd::CompositeInfo::LegacyComposite(info)) => {
                    write_legacy_composite(
                        writer,
                        info,
                        composite_node.parent_topological_paths.unwrap(),
                        composite_node.topological_path,
                        cmd.verbose,
                    )?;
                }
                Some(fdd::CompositeInfo::Composite(info)) => {
                    write_composite(
                        writer,
                        info,
                        composite_node.parent_topological_paths.unwrap(),
                        composite_node.topological_path,
                        cmd.verbose,
                    )?;
                }
                _ => {}
            }
        }
    }

    Ok(())
}

fn write_composite(
    writer: &mut dyn Write,
    composite: fdf::CompositeInfo,
    parent_topological_paths: Vec<Option<String>>,
    topological_path: Option<String>,
    verbose: bool,
) -> Result<()> {
    let spec = composite.spec.unwrap_or_default();
    let driver_match = composite.matched_driver.unwrap_or_default();
    if !verbose {
        writeln!(writer, "{}", spec.name.unwrap_or("".to_string()))?;
        return Ok(());
    }

    writeln!(writer, "{0: <9}: {1}", "Name", spec.name.unwrap_or("".to_string()))?;
    writeln!(
        writer,
        "{0: <9}: {1}",
        "Driver",
        driver_match
            .composite_driver
            .and_then(|composite_driver| composite_driver.driver_info)
            .and_then(|driver_info| driver_info.url)
            .unwrap_or("N/A".to_string())
    )?;
    writeln!(writer, "{0: <9}: {1}", "Device", topological_path.unwrap_or("N/A".to_string()))?;

    write_parent_nodes_info(
        writer,
        driver_match.primary_parent_index,
        driver_match.parent_names.unwrap_or_default(),
        parent_topological_paths,
    )?;

    writeln!(writer)?;
    Ok(())
}

fn write_legacy_composite(
    writer: &mut dyn Write,
    composite: fdl::CompositeInfo,
    parent_topological_paths: Vec<Option<String>>,
    topological_path: Option<String>,
    verbose: bool,
) -> Result<()> {
    let driver_match = composite.matched_driver.unwrap_or_default();
    if !verbose {
        writeln!(writer, "{}", composite.name.unwrap_or("".to_string()))?;
        return Ok(());
    }

    writeln!(writer, "{0: <9}: {1}", "Name", composite.name.unwrap_or("".to_string()))?;
    writeln!(writer, "{0: <9}: {1}", "Driver", driver_match.url.unwrap_or("N/A".to_string()))?;
    writeln!(writer, "{0: <9}: {1}", "Device", topological_path.unwrap_or("N/A".to_string()))?;

    write_legacy_composite_node_info(
        writer,
        composite.primary_fragment_index.unwrap_or(0) as usize,
        composite.fragments.unwrap_or_default(),
        composite.properties.unwrap_or_default(),
        parent_topological_paths,
    )?;

    writeln!(writer)?;
    Ok(())
}

fn write_legacy_composite_node_info(
    writer: &mut dyn Write,
    primary_fragment_index: usize,
    fragments: Vec<fdl::CompositeFragmentInfo>,
    properties: Vec<fdf::NodeProperty>,
    parent_topological_paths: Vec<Option<String>>,
) -> Result<()> {
    write_node_properties(&properties, writer)?;
    writeln!(writer, "{0: <10}: {1}", "Fragments", fragments.len())?;

    for (i, fragment) in fragments.into_iter().enumerate() {
        let primary_tag = if i == primary_fragment_index { "(Primary)" } else { "" };
        let topo_path = parent_topological_paths[i].clone();
        writeln!(
            writer,
            "{0: <1} {1} : {2} {3}",
            "Fragment",
            i,
            fragment.name.unwrap_or("".to_string()),
            primary_tag
        )?;

        writeln!(writer, "   {0: <1} : {1}", "Device", topo_path.unwrap_or("Unbound".to_string()))?;

        writeln!(writer, "   {0: <1} :", "Bind rules")?;
        let bind_rules = fragment.bind_rules.unwrap_or(vec![]);
        for rule in bind_rules {
            writeln!(writer, "     {:?}", rule)?;
        }
    }
    Ok(())
}

fn write_parent_nodes_info(
    writer: &mut dyn Write,
    primary_index: Option<u32>,
    parent_names: Vec<String>,
    parent_paths: Vec<Option<String>>,
) -> Result<()> {
    writeln!(writer, "{0: <9}: {1}", "Parents", parent_names.len())?;
    for (i, parent_name) in parent_names.into_iter().enumerate() {
        let primary_tag = if primary_index == Some(i as u32) { "(Primary)" } else { "" };
        writeln!(writer, "{0: <1} {1} : {2} {3}", "Parent", i, parent_name, primary_tag)?;

        writeln!(
            writer,
            "   {0: <1} : {1}",
            "Device",
            parent_paths[i].clone().unwrap_or("Unbound".to_string())
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use {
        super::*, fidl_fuchsia_driver_legacy::BindInstruction, fuchsia_async as fasync,
        std::io::Error,
    };

    pub struct TestWriteBuffer {
        pub content: String,
    }

    impl Write for TestWriteBuffer {
        fn write(&mut self, buf: &[u8]) -> Result<usize, Error> {
            self.content.push_str(std::str::from_utf8(buf).unwrap());
            Ok(buf.len())
        }

        fn flush(&mut self) -> Result<(), Error> {
            Ok(())
        }
    }

    fn gen_composite_property_data() -> Vec<fdf::NodeProperty> {
        vec![
            fdf::NodeProperty {
                key: fdf::NodePropertyKey::StringValue("avocet".to_string()),
                value: fdf::NodePropertyValue::IntValue(10),
            },
            fdf::NodeProperty {
                key: fdf::NodePropertyKey::StringValue("stilt".to_string()),
                value: fdf::NodePropertyValue::BoolValue(false),
            },
        ]
    }

    fn gen_legacy_composite_data() -> fdl::CompositeInfo {
        let test_fragments = vec![
            fdl::CompositeFragmentInfo {
                name: Some("sysmem".to_string()),
                bind_rules: Some(vec![BindInstruction { op: 1, arg: 30, debug: 0 }]),
                ..Default::default()
            },
            fdl::CompositeFragmentInfo {
                name: Some("acpi".to_string()),
                bind_rules: Some(vec![
                    BindInstruction { op: 2, arg: 50, debug: 0 },
                    BindInstruction { op: 1, arg: 30, debug: 0 },
                ]),
                ..Default::default()
            },
        ];

        let driver_match = fdf::DriverInfo {
            url: Some("fuchsia-boot:///#meta/waxwing.cm".to_string()),
            ..Default::default()
        };

        fdl::CompositeInfo {
            name: Some("composite_dev".to_string()),
            fragments: Some(test_fragments),
            properties: Some(gen_composite_property_data()),
            matched_driver: Some(driver_match),
            primary_fragment_index: Some(1),
            ..Default::default()
        }
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_composite_verbose() {
        let test_composite = fdf::CompositeInfo {
            spec: Some(fdf::CompositeNodeSpec {
                name: Some("composite_dev".to_string()),
                ..Default::default()
            }),
            matched_driver: Some(fdf::CompositeDriverMatch {
                composite_driver: Some(fdf::CompositeDriverInfo {
                    driver_info: Some(fdf::DriverInfo {
                        url: Some("fuchsia-boot:///#meta/waxwing.cm".to_string()),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                parent_names: Some(vec!["sysmem".to_string(), "acpi".to_string()]),
                primary_parent_index: Some(1),
                ..Default::default()
            }),
            ..Default::default()
        };

        let mut test_write_buffer = TestWriteBuffer { content: "".to_string() };
        write_composite(
            &mut test_write_buffer,
            test_composite,
            vec![Some("path/sysmem_dev".to_string()), Some("path/acpi_dev".to_string())],
            Some("dev/sys/composite_dev".to_string()),
            true,
        )
        .unwrap();
        assert_eq!(
            include_str!("../../../tests/golden/list_composites_verbose"),
            test_write_buffer.content
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_composite_verbose_empty_fields() {
        let test_composite = fdf::CompositeInfo {
            spec: Some(fdf::CompositeNodeSpec {
                name: Some("composite_dev".to_string()),
                ..Default::default()
            }),
            matched_driver: Some(fdf::CompositeDriverMatch {
                composite_driver: Some(fdf::CompositeDriverInfo {
                    composite_name: Some("composite_name".to_string()),
                    driver_info: Some(fdf::DriverInfo {
                        url: Some("fuchsia-boot:///#meta/waxwing.cm".to_string()),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                parent_names: Some(vec!["sysmem".to_string(), "acpi".to_string()]),
                primary_parent_index: Some(1),
                ..Default::default()
            }),
            ..Default::default()
        };

        let mut test_write_buffer = TestWriteBuffer { content: "".to_string() };
        write_composite(
            &mut test_write_buffer,
            test_composite,
            vec![None, None],
            Some("dev/sys/composite_dev".to_string()),
            true,
        )
        .unwrap();
        println!("{}", test_write_buffer.content);

        assert_eq!(
            include_str!("../../../tests/golden/list_composites_verbose_empty_fields"),
            test_write_buffer.content
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_legacy_composite_verbose() {
        let mut test_write_buffer = TestWriteBuffer { content: "".to_string() };
        write_legacy_composite(
            &mut test_write_buffer,
            gen_legacy_composite_data(),
            vec![Some("sysmem_dev".to_string()), Some("acpi_dev".to_string())],
            Some("dev/sys/composite_dev".to_string()),
            true,
        )
        .unwrap();
        assert_eq!(
            include_str!("../../../tests/golden/list_legacy_composites_verbose"),
            test_write_buffer.content
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_legacy_composite_nonverbose() {
        let mut test_write_buffer = TestWriteBuffer { content: "".to_string() };
        write_legacy_composite(
            &mut test_write_buffer,
            gen_legacy_composite_data(),
            vec![Some("sysmem_dev".to_string()), Some("acpi_dev".to_string())],
            Some("dev/sys/composite_dev".to_string()),
            false,
        )
        .unwrap();

        let expected_output = "composite_dev\n";
        assert_eq!(expected_output, test_write_buffer.content);
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_composite_empty_fields() {
        let test_fragments = vec![
            fdl::CompositeFragmentInfo {
                name: Some("sysmem".to_string()),
                bind_rules: Some(vec![BindInstruction { op: 1, arg: 30, debug: 0 }]),
                ..Default::default()
            },
            fdl::CompositeFragmentInfo {
                name: Some("acpi".to_string()),
                bind_rules: Some(vec![
                    BindInstruction { op: 2, arg: 50, debug: 0 },
                    BindInstruction { op: 1, arg: 30, debug: 0 },
                ]),
                ..Default::default()
            },
        ];

        let test_composite = fdl::CompositeInfo {
            name: Some("composite_dev".to_string()),
            fragments: Some(test_fragments),
            properties: Some(gen_composite_property_data()),
            matched_driver: None,
            primary_fragment_index: Some(1),
            ..Default::default()
        };

        let mut test_write_buffer = TestWriteBuffer { content: "".to_string() };
        write_legacy_composite(
            &mut test_write_buffer,
            test_composite,
            vec![None, None],
            None,
            true,
        )
        .unwrap();
        assert_eq!(
            include_str!("../../../tests/golden/list_legacy_composites_verbose_empty_fields"),
            test_write_buffer.content
        );
    }
}
