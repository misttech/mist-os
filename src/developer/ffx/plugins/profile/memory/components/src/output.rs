// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use attribution_processing::kernel_statistics::KernelStatistics;
use attribution_processing::summary::{MemorySummary, PrincipalSummary, VmoSummary};
use attribution_processing::ZXName;

use prettytable::{row, table, Table};

pub fn write_summary(
    f: &mut dyn std::io::Write,
    csv: bool,
    value: &MemorySummary,
    kernel_statistics: &KernelStatistics,
) -> std::io::Result<()> {
    if csv {
        let mut csv_writer = csv::Writer::from_writer(f);
        csv_writer.write_record(&[
            "attributor",
            "principal",
            "vmo",
            "committed_private",
            "populated_private",
            "committed_scaled",
            "populated_scaled",
            "committed_total",
            "populated_total",
        ])?;
        for principal in &value.principals {
            write_summary_principal_csv(&mut csv_writer, principal)?;
        }
    } else {
        write_summary_kernel_stats(f, kernel_statistics)?;
        for principal in &value.principals {
            writeln!(f)?;
            writeln!(f)?;
            write_summary_principal(f, principal)?;
        }
    }
    Ok(())
}

fn write_summary_principal(
    f: &mut dyn std::io::Write,
    value: &PrincipalSummary,
) -> std::io::Result<()> {
    let format = prettytable::format::FormatBuilder::new().padding(1, 1).build();
    let mut tbl = table!(
        ["Principal name:", &value.name],
        ["Principal id:", &value.id.to_string()],
        [
            "Principal type:",
            match value.principal_type.as_str() {
                "R" => "Runnable",
                "P" => "Part",
                o => o,
            }
        ]
    );

    if let Some(parent) = &value.attributor {
        tbl.add_row(row!["Attributor:", parent]);
    }

    if !value.processes.is_empty() {
        tbl.add_row(row!["Processes:", value.processes.join(", ")]);
    }
    tbl.set_format(format);
    tbl.print(f)?;

    let mut vmos: Vec<(&ZXName, &VmoSummary)> = value.vmos.iter().collect();
    vmos.sort_by_key(|(_, v)| -(v.populated_total as i64));
    let mut tbl = Table::new();
    tbl.add_row(
        row![bc -> "VMO name", bc->"Count", bH2c->"Private", bH2c->"Scaled", bH2c->"Total"],
    );
    tbl.add_row(row![bH2 -> "", bc->"Committed", bc->"Populated", bc->"Committed", bc->"Populated", bc->"Committed", bc->"Populated"]);
    tbl.add_row(row![
        "Total",
        "",
        r->format_bytes(value.committed_private as f64),
        r->format_bytes(value.populated_private as f64),
        r->format_bytes(value.committed_scaled),
        r->format_bytes(value.populated_scaled),
        r->format_bytes(value.committed_total as f64),
        r->format_bytes(value.populated_total as f64)
    ]);
    tbl.add_row(row![]);
    for (name, vmo) in vmos {
        tbl.add_row(row![
            name,
            r->vmo.count,
            r->format_bytes(vmo.committed_private as f64),
            r->format_bytes(vmo.populated_private as f64),
            r->format_bytes(vmo.committed_scaled),
            r->format_bytes(vmo.populated_scaled),
            r->format_bytes(vmo.committed_total as f64),
            r->format_bytes(vmo.populated_total as f64)
        ]);
    }
    tbl.set_format(format);
    tbl.print(f)?;
    Ok(())
}

fn write_summary_kernel_stats(
    w: &mut dyn std::io::Write,
    value: &KernelStatistics,
) -> std::io::Result<()> {
    writeln!(
        w,
        "Total memory: {}",
        format_bytes(value.memory_statistics.total_bytes.unwrap() as f64)
    )?;
    writeln!(
        w,
        "Free memory: {}",
        format_bytes(value.memory_statistics.free_bytes.unwrap() as f64)
    )?;
    let kernel_total = value.memory_statistics.wired_bytes.unwrap()
        + value.memory_statistics.total_heap_bytes.unwrap()
        + value.memory_statistics.mmu_overhead_bytes.unwrap()
        + value.memory_statistics.ipc_bytes.unwrap();
    writeln!(w, "Kernel:    {}", format_bytes(kernel_total as f64))?;
    writeln!(
        w,
        "    wired: {}",
        format_bytes(value.memory_statistics.wired_bytes.unwrap() as f64)
    )?;
    writeln!(w, "    vmo:   {}", format_bytes(value.memory_statistics.vmo_bytes.unwrap() as f64))?;
    writeln!(
        w,
        "    heap:  {}",
        format_bytes(value.memory_statistics.total_heap_bytes.unwrap() as f64)
    )?;
    writeln!(
        w,
        "    mmu:   {}",
        format_bytes(value.memory_statistics.mmu_overhead_bytes.unwrap() as f64)
    )?;
    writeln!(w, "    ipc:   {}", format_bytes(value.memory_statistics.ipc_bytes.unwrap() as f64))?;
    if let Some(zram_bytes) = value.memory_statistics.zram_bytes {
        writeln!(w, "    zram:  {}", format_bytes(zram_bytes as f64))?;
    }
    writeln!(
        w,
        "    other: {}",
        format_bytes(value.memory_statistics.other_bytes.unwrap() as f64)
    )?;
    writeln!(w, "  including:")?;
    writeln!(
        w,
        "    vmo_reclaim_total_bytes:        {}",
        format_bytes(value.memory_statistics.vmo_reclaim_total_bytes.unwrap() as f64)
    )?;
    writeln!(
        w,
        "    vmo_reclaim_newest_bytes:       {}",
        format_bytes(value.memory_statistics.vmo_reclaim_newest_bytes.unwrap() as f64)
    )?;
    writeln!(
        w,
        "    vmo_reclaim_oldest_bytes:       {}",
        format_bytes(value.memory_statistics.vmo_reclaim_oldest_bytes.unwrap() as f64)
    )?;
    writeln!(
        w,
        "    vmo_reclaim_disabled_bytes:     {}",
        format_bytes(value.memory_statistics.vmo_reclaim_disabled_bytes.unwrap() as f64)
    )?;
    writeln!(
        w,
        "    vmo_discardable_locked_bytes:   {}",
        format_bytes(value.memory_statistics.vmo_discardable_locked_bytes.unwrap() as f64)
    )?;
    writeln!(
        w,
        "    vmo_discardable_unlocked_bytes: {}",
        format_bytes(value.memory_statistics.vmo_discardable_unlocked_bytes.unwrap() as f64)
    )
}

fn write_summary_principal_csv<W: std::io::Write>(
    csv_writer: &mut csv::Writer<W>,
    value: &PrincipalSummary,
) -> std::io::Result<()> {
    let mut vmos: Vec<(&ZXName, &VmoSummary)> = value.vmos.iter().collect();
    vmos.sort_by_key(|(_, v)| -(v.populated_total as i64));
    for (name, vmo) in vmos {
        csv_writer.write_record(&[
            value.attributor.clone().unwrap_or_default(),
            value.name.to_string(),
            name.to_string(),
            vmo.committed_private.to_string(),
            vmo.populated_private.to_string(),
            vmo.committed_scaled.to_string(),
            vmo.populated_scaled.to_string(),
            vmo.committed_total.to_string(),
            vmo.populated_total.to_string(),
        ])?;
    }

    csv_writer.write_record(&[
        value.attributor.clone().unwrap_or_default(),
        value.name.to_string(),
        "Total".to_string(),
        value.committed_private.to_string(),
        value.populated_private.to_string(),
        value.committed_scaled.to_string(),
        value.populated_scaled.to_string(),
        value.committed_total.to_string(),
        value.populated_total.to_string(),
    ])?;
    Ok(())
}

pub fn format_bytes(bytes: f64) -> String {
    if bytes < 1024.0 {
        format!("{:0.2} B", bytes)
    } else if bytes / 1024.0 < 1024.0 {
        format!("{:0.2} KiB", bytes / 1024.0)
    } else {
        format!("{:0.2} MiB", bytes / (1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::io::BufWriter;

    #[test]
    fn principal_output_string() {
        let po = PrincipalSummary {
            id: 42,
            name: String::from("test_name"),
            principal_type: String::from("R"),
            committed_private: 100,
            committed_scaled: 200.0,
            committed_total: 300,
            populated_private: 400,
            populated_scaled: 500.0,
            populated_total: 600,
            attributor: None,
            processes: vec![String::from("proc_a"), String::from("proc_b")],
            vmos: HashMap::from([(
                ZXName::from_string_lossy("[scudo]"),
                VmoSummary {
                    count: 42,
                    committed_private: 10,
                    committed_scaled: 20.0,
                    committed_total: 30,
                    populated_private: 40,
                    populated_scaled: 50.0,
                    populated_total: 60,
                },
            )]),
        };

        let actual_output = {
            let mut buf = BufWriter::new(Vec::new());
            write_summary_principal(&mut buf, &po).unwrap();

            let bytes = buf.into_inner().unwrap();
            String::from_utf8(bytes).unwrap()
        };

        let expected_output = r#" Principal name:  test_name |
 Principal id:    42 |
 Principal type:  Runnable |
 Processes:       proc_a, proc_b |
 VMO name  Count        Private                Scaled                Total         |
                  Committed  Populated  Committed  Populated  Committed  Populated |
 Total             100.00 B   400.00 B   200.00 B   500.00 B   300.00 B   600.00 B |
                                                                          |
 [scudo]      42    10.00 B    40.00 B    20.00 B    50.00 B    30.00 B    60.00 B |
"#;
        pretty_assertions::assert_eq!(actual_output, expected_output.replace("|", ""));
    }

    #[test]
    fn test_write_summary() {
        let principal = PrincipalSummary {
            id: 42,
            name: String::from("test_name"),
            principal_type: String::from("R"),
            committed_private: 1,
            committed_scaled: 2.0,
            committed_total: 3,
            populated_private: 4,
            populated_scaled: 5.0,
            populated_total: 6,
            attributor: Some(String::from("mr,freeze")),
            processes: vec![],
            vmos: HashMap::from([(
                ZXName::from_string_lossy("[scudo]"),
                VmoSummary {
                    count: 42,
                    committed_private: 10,
                    committed_scaled: 20.0,
                    committed_total: 30,
                    populated_private: 40,
                    populated_scaled: 50.0,
                    populated_total: 60,
                },
            )]),
        };
        let summary = MemorySummary { principals: vec![principal], undigested: 1 };

        let actual_output = {
            let mut buf = BufWriter::new(Vec::new());
            write_summary(&mut buf, true, &summary, &Default::default()).unwrap();
            let bytes = buf.into_inner().unwrap();
            String::from_utf8(bytes).unwrap()
        };
        let expected_output = r#"attributor,principal,vmo,committed_private,populated_private,committed_scaled,populated_scaled,committed_total,populated_total
"mr,freeze",test_name,[scudo],10,40,20,50,30,60
"mr,freeze",test_name,Total,1,4,2,5,3,6
"#;
        pretty_assertions::assert_eq!(actual_output, expected_output);
    }

    #[test]
    fn test_write_summary_scarce_info() {
        let principal = PrincipalSummary {
            id: 42,
            name: String::from("test_name"),
            principal_type: String::from("R"),
            committed_private: 1,
            committed_scaled: 2.0,
            committed_total: 3,
            populated_private: 4,
            populated_scaled: 5.0,
            populated_total: 6,
            attributor: None,
            processes: vec![],
            vmos: HashMap::from([]),
        };
        let summary = MemorySummary { principals: vec![principal], undigested: 1 };

        let actual_output = {
            let mut buf = BufWriter::new(Vec::new());
            write_summary(&mut buf, true, &summary, &Default::default()).unwrap();
            let bytes = buf.into_inner().unwrap();
            String::from_utf8(bytes).unwrap()
        };
        let expected_output = r#"attributor,principal,vmo,committed_private,populated_private,committed_scaled,populated_scaled,committed_total,populated_total
,test_name,Total,1,4,2,5,3,6
"#;
        pretty_assertions::assert_eq!(actual_output, expected_output);
    }
}
