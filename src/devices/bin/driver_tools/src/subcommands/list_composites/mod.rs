// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod args;

use {
    crate::common::write_node_properties,
    anyhow::{Context, Result},
    args::ListCompositesCommand,
    fidl_fuchsia_driver_development as fdd,
    std::io::Write,
};

pub async fn list_composites(
    cmd: ListCompositesCommand,
    writer: &mut dyn Write,
    proxy: fdd::DriverDevelopmentProxy,
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

        for composite in composite_list {
            write_composite(writer, composite, cmd.verbose)?;
        }
    }

    Ok(())
}

fn write_composite(
    writer: &mut dyn Write,
    composite: fdd::CompositeInfo,
    verbose: bool,
) -> Result<()> {
    if !verbose {
        writeln!(writer, "{}", composite.name.unwrap_or("".to_string()))?;
        return Ok(());
    }

    writeln!(writer, "{0: <9}: {1}", "Name", composite.name.unwrap_or("".to_string()))?;
    writeln!(writer, "{0: <9}: {1}", "Driver", composite.driver.unwrap_or("N/A".to_string()))?;
    writeln!(
        writer,
        "{0: <9}: {1}",
        "Device",
        composite.topological_path.unwrap_or("N/A".to_string())
    )?;

    match composite.node_info {
        Some(fdd::CompositeNodeInfo::Legacy(info)) => {
            write_legacy_composite_node_info(writer, composite.primary_index, info)?;
        }
        Some(fdd::CompositeNodeInfo::Parents(parents)) => {
            write_parent_nodes_info(writer, composite.primary_index, parents)?;
        }
        None => {}
    }

    writeln!(writer)?;
    Ok(())
}

fn write_legacy_composite_node_info(
    writer: &mut dyn Write,
    primary_index: Option<u32>,
    composite: fdd::LegacyCompositeNodeInfo,
) -> Result<()> {
    write_node_properties(&composite.properties.unwrap_or(vec![]), writer)?;

    let fragments = composite.fragments.unwrap_or(vec![]);
    writeln!(writer, "{0: <10}: {1}", "Fragments", fragments.len())?;

    for (i, fragment) in fragments.into_iter().enumerate() {
        let primary_tag = if primary_index == Some(i as u32) { "(Primary)" } else { "" };
        writeln!(
            writer,
            "{0: <1} {1} : {2} {3}",
            "Fragment",
            i,
            fragment.name.unwrap_or("".to_string()),
            primary_tag
        )?;

        writeln!(
            writer,
            "   {0: <1} : {1}",
            "Device",
            fragment.device.unwrap_or("Unbound".to_string())
        )?;

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
    parents: Vec<fdd::CompositeParentNodeInfo>,
) -> Result<()> {
    writeln!(writer, "{0: <9}: {1}", "Parents", parents.len())?;
    for (i, parent) in parents.into_iter().enumerate() {
        let primary_tag = if primary_index == Some(i as u32) { "(Primary)" } else { "" };
        writeln!(
            writer,
            "{0: <1} {1} : {2} {3}",
            "Parent",
            i,
            parent.name.unwrap_or("".to_string()),
            primary_tag
        )?;

        writeln!(
            writer,
            "   {0: <1} : {1}",
            "Device",
            parent.device.unwrap_or("Unbound".to_string())
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use {
        super::*, fidl_fuchsia_device_manager::BindInstruction,
        fidl_fuchsia_driver_framework as fdf, fuchsia_async as fasync, std::io::Error,
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

    fn gen_legacy_composite_data() -> fdd::CompositeInfo {
        let test_fragments = vec![
            fdd::LegacyCompositeFragmentInfo {
                name: Some("sysmem".to_string()),
                bind_rules: Some(vec![BindInstruction { op: 1, arg: 30, debug: 0 }]),
                device: Some("sysmem_dev".to_string()),
                ..Default::default()
            },
            fdd::LegacyCompositeFragmentInfo {
                name: Some("acpi".to_string()),
                bind_rules: Some(vec![
                    BindInstruction { op: 2, arg: 50, debug: 0 },
                    BindInstruction { op: 1, arg: 30, debug: 0 },
                ]),
                device: Some("acpi_dev".to_string()),
                ..Default::default()
            },
        ];

        let legacy_info = fdd::LegacyCompositeNodeInfo {
            fragments: Some(test_fragments),
            properties: Some(gen_composite_property_data()),
            ..Default::default()
        };

        fdd::CompositeInfo {
            name: Some("composite_dev".to_string()),
            driver: Some("fuchsia-boot:///#driver/waxwing.so".to_string()),
            topological_path: Some("dev/sys/composite_dev".to_string()),
            primary_index: Some(1),
            node_info: Some(fdd::CompositeNodeInfo::Legacy(legacy_info)),
            ..Default::default()
        }
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_composite_verbose() {
        let test_parents = vec![
            fdd::CompositeParentNodeInfo {
                name: Some("sysmem".to_string()),
                device: Some("path/sysmem_dev".to_string()),
                ..Default::default()
            },
            fdd::CompositeParentNodeInfo {
                name: Some("acpi".to_string()),
                device: Some("path/acpi_dev".to_string()),
                ..Default::default()
            },
        ];

        let test_composite = fdd::CompositeInfo {
            name: Some("composite_dev".to_string()),
            driver: Some("fuchsia-boot:///#driver/waxwing.so".to_string()),
            topological_path: Some("dev/sys/composite_dev".to_string()),
            primary_index: Some(1),
            node_info: Some(fdd::CompositeNodeInfo::Parents(test_parents)),
            ..Default::default()
        };

        let mut test_write_buffer = TestWriteBuffer { content: "".to_string() };
        write_composite(&mut test_write_buffer, test_composite, true).unwrap();
        assert_eq!(
            include_str!("../../../tests/golden/list_composites_verbose"),
            test_write_buffer.content
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_composite_verbose_empty_fields() {
        let test_parents = vec![
            fdd::CompositeParentNodeInfo { name: Some("sysmem".to_string()), ..Default::default() },
            fdd::CompositeParentNodeInfo { name: Some("acpi".to_string()), ..Default::default() },
        ];

        let test_composite = fdd::CompositeInfo {
            name: Some("composite_dev".to_string()),
            driver: Some("fuchsia-boot:///#driver/waxwing.so".to_string()),
            topological_path: Some("dev/sys/composite_dev".to_string()),
            primary_index: Some(1),
            node_info: Some(fdd::CompositeNodeInfo::Parents(test_parents)),
            ..Default::default()
        };

        let mut test_write_buffer = TestWriteBuffer { content: "".to_string() };
        write_composite(&mut test_write_buffer, test_composite, true).unwrap();
        println!("{}", test_write_buffer.content);

        assert_eq!(
            include_str!("../../../tests/golden/list_composites_verbose_empty_fields"),
            test_write_buffer.content
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_legacy_composite_verbose() {
        let mut test_write_buffer = TestWriteBuffer { content: "".to_string() };
        write_composite(&mut test_write_buffer, gen_legacy_composite_data(), true).unwrap();
        assert_eq!(
            include_str!("../../../tests/golden/list_legacy_composites_verbose"),
            test_write_buffer.content
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_legacy_composite_nonverbose() {
        let mut test_write_buffer = TestWriteBuffer { content: "".to_string() };
        write_composite(&mut test_write_buffer, gen_legacy_composite_data(), false).unwrap();

        let expected_output = "composite_dev\n";
        assert_eq!(expected_output, test_write_buffer.content);
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_composite_empty_fields() {
        let test_fragments = vec![
            fdd::LegacyCompositeFragmentInfo {
                name: Some("sysmem".to_string()),
                bind_rules: Some(vec![BindInstruction { op: 1, arg: 30, debug: 0 }]),
                device: None,
                ..Default::default()
            },
            fdd::LegacyCompositeFragmentInfo {
                name: Some("acpi".to_string()),
                bind_rules: Some(vec![
                    BindInstruction { op: 2, arg: 50, debug: 0 },
                    BindInstruction { op: 1, arg: 30, debug: 0 },
                ]),
                device: None,
                ..Default::default()
            },
        ];

        let legacy_info = fdd::LegacyCompositeNodeInfo {
            fragments: Some(test_fragments),
            properties: Some(gen_composite_property_data()),
            ..Default::default()
        };

        let test_composite = fdd::CompositeInfo {
            name: Some("composite_dev".to_string()),
            driver: None,
            primary_index: Some(1),
            node_info: Some(fdd::CompositeNodeInfo::Legacy(legacy_info)),
            ..Default::default()
        };

        let mut test_write_buffer = TestWriteBuffer { content: "".to_string() };
        write_composite(&mut test_write_buffer, test_composite, true).unwrap();
        assert_eq!(
            include_str!("../../../tests/golden/list_legacy_composites_verbose_empty_fields"),
            test_write_buffer.content
        );
    }
}
