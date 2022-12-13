// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::bucket::Bucket, crate::digest, crate::plugin_output::ProcessesMemoryUsage,
    crate::ProfileMemoryOutput, anyhow::Result, digest::processed, ffx_writer::Writer,
    std::io::Write,
};

/// Transforms nanoseconds into seconds, because tools that use CSV expect seconds.
fn nanoseconds_to_seconds(time: u64) -> u64 {
    time / 1000000000
}

/// Write to `w` a csv presentation of `processes`.
fn write_processes_digest(w: &mut Writer, processes: &ProcessesMemoryUsage) -> Result<()> {
    for process in &processes.process_data {
        writeln!(
            w,
            "{},{},{},{},{},{}",
            nanoseconds_to_seconds(processes.capture_time),
            process.koid,
            process.name,
            process.memory.private,
            process.memory.scaled,
            process.memory.total
        )?;
    }
    Ok(())
}

/// Write to `w` a csv presentation of the buckets
pub fn write_csv_buckets(w: &mut Writer, buckets: &Vec<Bucket>, capture_time: u64) -> Result<()> {
    for bucket in buckets {
        writeln!(w, "{},{},{}", nanoseconds_to_seconds(capture_time), bucket.name, bucket.size,)?;
    }
    Ok(())
}

/// Write to `w` a csv presentation of `digest`.
/// If `bucketize` is true, output only the bucket data. Otherwise, output only the process data.
/// Outputting more than just the process data (e.g. `total_committed_bytes_in_vmos`)
/// is out of the scope of the CSV output.
fn write_complete_digest(w: &mut Writer, digest: processed::Digest, bucketize: bool) -> Result<()> {
    if bucketize {
        write_csv_buckets(w, &digest.buckets, digest.time)
    } else {
        write_processes_digest(
            w,
            &ProcessesMemoryUsage { process_data: digest.processes, capture_time: digest.time },
        )
    }
}

/// Write to `w` a csv presentation of `output`.
pub fn write_csv_output<'a>(
    w: &mut Writer,
    internal_output: ProfileMemoryOutput,
    bucketize: bool,
) -> Result<()> {
    match internal_output {
        ProfileMemoryOutput::CompleteDigest(digest) => write_complete_digest(w, digest, bucketize),
        ProfileMemoryOutput::ProcessDigest(processes_digest) => {
            write_processes_digest(w, &processes_digest)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin_output::ProcessesMemoryUsage;
    use crate::processed::Kernel;
    use crate::processed::RetainedMemory;
    use crate::ProfileMemoryOutput::CompleteDigest;
    use crate::ProfileMemoryOutput::ProcessDigest;
    use std::collections::HashMap;
    use std::collections::HashSet;

    fn process_data_for_test() -> crate::ProfileMemoryOutput {
        ProcessDigest(ProcessesMemoryUsage {
            capture_time: 123000111222,
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
    fn write_csv_output_processes_test() {
        let mut writer = Writer::new_test(None);
        let _ = write_csv_output(&mut writer, process_data_for_test(), false);
        let actual_output = writer.test_output().unwrap();
        let expected_output = "123,4,P,11,22,33\n";
        pretty_assertions::assert_eq!(actual_output, *expected_output);
    }

    fn bucket_data_for_test() -> crate::ProfileMemoryOutput {
        CompleteDigest(processed::Digest {
            time: 567000111222,
            total_committed_bytes_in_vmos: 0,
            kernel: Kernel::default(),
            processes: vec![],
            vmos: vec![],
            buckets: vec![
                Bucket { name: "Bucket0".to_string(), size: 42 },
                Bucket { name: "Bucket1".to_string(), size: 43 },
            ],
        })
    }

    #[test]
    fn write_csv_output_buckets_test() {
        let mut writer = Writer::new_test(None);
        let _ = write_csv_output(&mut writer, bucket_data_for_test(), true);
        let actual_output = writer.test_output().unwrap();
        let expected_output = "567,Bucket0,42\n567,Bucket1,43\n";
        pretty_assertions::assert_eq!(actual_output, *expected_output);
    }
}
