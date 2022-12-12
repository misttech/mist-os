// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::digest, crate::plugin_output::ProcessesMemoryUsage, crate::ProfileMemoryOutput,
    anyhow::Result, digest::processed, ffx_writer::Writer, std::io::Write,
};

/// Print to `w` a csv presentation of `processes`.
fn print_processes_digest(w: &mut Writer, processes: ProcessesMemoryUsage) -> Result<()> {
    for process in processes.process_data {
        writeln!(
            w,
            "{},{},{},{},{},{}",
            processes.capture_time,
            process.koid,
            process.name,
            process.memory.private,
            process.memory.scaled,
            process.memory.total
        )?;
    }
    Ok(())
}

/// Print to `w` a human-readable presentation of `digest`.
/// Outputting more than just the process data (e.g. `total_committed_bytes_in_vmos`)
/// is out of the scope of the CSV output.
fn print_complete_digest(w: &mut Writer, digest: processed::Digest) -> Result<()> {
    print_processes_digest(
        w,
        ProcessesMemoryUsage { process_data: digest.processes, capture_time: digest.time },
    )?;
    Ok(())
}

/// Print to `w` a csv presentation of `output`.
pub fn write_csv_output<'a>(w: &mut Writer, output: ProfileMemoryOutput) -> Result<()> {
    match output {
        ProfileMemoryOutput::CompleteDigest(digest) => print_complete_digest(w, digest),
        ProfileMemoryOutput::ProcessDigest(processes_digest) => {
            print_processes_digest(w, processes_digest)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin_output::ProcessesMemoryUsage;
    use crate::processed::RetainedMemory;
    use crate::ProfileMemoryOutput::ProcessDigest;
    use std::collections::HashMap;
    use std::collections::HashSet;

    fn data_for_test() -> crate::ProfileMemoryOutput {
        ProcessDigest(ProcessesMemoryUsage {
            capture_time: 123,
            process_data: vec![processed::Process {
                koid: 4,
                name: "P".to_string(),
                memory: RetainedMemory { private: 11, scaled: 22, total: 33, vmos: vec![] },
                name_to_memory: {
                    let mut result = HashMap::new();
                    result.insert(
                        "vmoC".to_string(),
                        processed::RetainedMemory {
                            private: 4444,
                            scaled: 55555,
                            total: 666666,
                            vmos: vec![],
                        },
                    );
                    result.insert(
                        "vmoB".to_string(),
                        processed::RetainedMemory {
                            private: 4444,
                            scaled: 55555,
                            total: 666666,
                            vmos: vec![],
                        },
                    );
                    result.insert(
                        "vmoA".to_string(),
                        processed::RetainedMemory {
                            private: 44444,
                            scaled: 555555,
                            total: 6666666,
                            vmos: vec![],
                        },
                    );
                    result
                },
                vmos: HashSet::new(),
            }],
        })
    }

    #[test]
    fn write_csv_output_test() {
        let mut writer = Writer::new_test(None);
        let _ = write_csv_output(&mut writer, data_for_test());
        let actual_output = writer.test_output().unwrap();
        let expected_output = "123,4,P,11,22,33\n";
        pretty_assertions::assert_eq!(actual_output, *expected_output);
    }
}
