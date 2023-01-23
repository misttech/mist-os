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
    writer: &mut impl Write,
    proxy: fdd::DriverDevelopmentProxy,
) -> Result<()> {
    let (iterator, iterator_server) =
        fidl::endpoints::create_proxy::<fdd::CompositeInfoIteratorMarker>()?;
    proxy.get_composite_info(iterator_server).context("GetCompositeInfo() failed")?;

    if proxy.is_dfv2().await? {
        writeln!(writer, "DFv2 composites are currently not supported. See fxb/119947.")?;
        return Ok(());
    }

    loop {
        let composite_list =
            iterator.get_next().await.context("CompositeInfoIterator GetNext() failed")?;

        match composite_list {
            fdd::CompositeList::Dfv1Composites(composites) => {
                if composites.is_empty() {
                    break;
                }

                for composite in composites {
                    write_dfv1_composite(writer, composite, cmd.verbose)?;
                }
            }
            fdd::CompositeList::Dfv2Composites(_) => {
                // TODO(fxb/119947): Implement support for DFv2.
                writeln!(writer, "DFv2 composites are currently not supported. See fxb/119947.")?;
                break;
            }
        }
    }

    Ok(())
}

fn write_dfv1_composite(
    writer: &mut impl Write,
    composite: fdd::Dfv1CompositeInfo,
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

    write_node_properties(&composite.properties.unwrap_or(vec![]), writer)?;

    let fragments = composite.fragments.unwrap_or(vec![]);
    writeln!(writer, "{0: <10}: {1}", "Fragments", fragments.len())?;

    for (i, fragment) in fragments.into_iter().enumerate() {
        let primary_tag = if composite.primary_index == Some(i as u32) { "(Primary)" } else { "" };
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

        writeln!(writer, "   {0: <1} : Bytecode Version 1", "Bind rules")?;
        let bind_rules = fragment.bind_rules.unwrap_or(vec![]);
        for rule in bind_rules {
            writeln!(writer, "     {:?}", rule)?;
        }
    }

    writeln!(writer)?;
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
                key: Some(fdf::NodePropertyKey::StringValue("avocet".to_string())),
                value: Some(fdf::NodePropertyValue::IntValue(10)),
                ..fdf::NodeProperty::EMPTY
            },
            fdf::NodeProperty {
                key: Some(fdf::NodePropertyKey::StringValue("stilt".to_string())),
                value: Some(fdf::NodePropertyValue::BoolValue(false)),
                ..fdf::NodeProperty::EMPTY
            },
        ]
    }

    fn gen_composite_data() -> fdd::Dfv1CompositeInfo {
        let test_fragments = vec![
            fdd::Dfv1CompositeFragmentInfo {
                name: Some("sysmem".to_string()),
                bind_rules: Some(vec![BindInstruction { op: 1, arg: 30, debug: 0 }]),
                device: Some("sysmem_dev".to_string()),
                ..fdd::Dfv1CompositeFragmentInfo::EMPTY
            },
            fdd::Dfv1CompositeFragmentInfo {
                name: Some("acpi".to_string()),
                bind_rules: Some(vec![
                    BindInstruction { op: 2, arg: 50, debug: 0 },
                    BindInstruction { op: 1, arg: 30, debug: 0 },
                ]),
                device: Some("acpi_dev".to_string()),
                ..fdd::Dfv1CompositeFragmentInfo::EMPTY
            },
        ];

        fdd::Dfv1CompositeInfo {
            name: Some("composite_dev".to_string()),
            driver: Some("fuchsia-boot:///#driver/waxwing.so".to_string()),
            topological_path: Some("dev/sys/composite_dev".to_string()),
            primary_index: Some(1),
            fragments: Some(test_fragments),
            properties: Some(gen_composite_property_data()),
            ..fdd::Dfv1CompositeInfo::EMPTY
        }
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_composite_verbose() {
        let mut test_write_buffer = TestWriteBuffer { content: "".to_string() };
        write_dfv1_composite(&mut test_write_buffer, gen_composite_data(), true).unwrap();
        assert_eq!(
            include_str!("../../../tests/golden/list_composites_verbose"),
            test_write_buffer.content
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_composite_nonverbose() {
        let mut test_write_buffer = TestWriteBuffer { content: "".to_string() };
        write_dfv1_composite(&mut test_write_buffer, gen_composite_data(), false).unwrap();

        let expected_output = "composite_dev\n";
        assert_eq!(expected_output, test_write_buffer.content);
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_composite_empty_fields() {
        let test_fragments = vec![
            fdd::Dfv1CompositeFragmentInfo {
                name: Some("sysmem".to_string()),
                bind_rules: Some(vec![BindInstruction { op: 1, arg: 30, debug: 0 }]),
                device: None,
                ..fdd::Dfv1CompositeFragmentInfo::EMPTY
            },
            fdd::Dfv1CompositeFragmentInfo {
                name: Some("acpi".to_string()),
                bind_rules: Some(vec![
                    BindInstruction { op: 2, arg: 50, debug: 0 },
                    BindInstruction { op: 1, arg: 30, debug: 0 },
                ]),
                device: None,
                ..fdd::Dfv1CompositeFragmentInfo::EMPTY
            },
        ];

        let test_composite = fdd::Dfv1CompositeInfo {
            name: Some("composite_dev".to_string()),
            driver: None,
            primary_index: Some(1),
            properties: Some(gen_composite_property_data()),
            fragments: Some(test_fragments),
            ..fdd::Dfv1CompositeInfo::EMPTY
        };

        let mut test_write_buffer = TestWriteBuffer { content: "".to_string() };
        write_dfv1_composite(&mut test_write_buffer, test_composite, true).unwrap();
        assert_eq!(
            include_str!("../../../tests/golden/list_composites_verbose_empty_fields"),
            test_write_buffer.content
        );
    }
}
