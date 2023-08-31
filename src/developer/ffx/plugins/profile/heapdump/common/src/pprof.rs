// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Result,
    heapdump_snapshot::Snapshot,
    itertools::Itertools,
    prost::Message,
    std::collections::hash_map::{Entry, HashMap},
    std::io::Write,
};

fn build_profile(snapshot: &Snapshot, with_tags: bool) -> Result<pprof::Profile> {
    let mut st = pprof::StringTableBuilder::default();

    let mut pprof = pprof::Profile {
        sample_type: vec![
            pprof::ValueType { r#type: st.intern("objects"), unit: st.intern("count") },
            pprof::ValueType { r#type: st.intern("allocated"), unit: st.intern("bytes") },
        ],
        ..Default::default()
    };

    // Build the Mappings with all the executable regions listed in the snapshot and obtain an
    // object that resolves arbitrary program addresses into the corresponding module IDs.
    let module_map = {
        let mut builder = pprof::ModuleMapBuilder::default();
        for (address, info) in &snapshot.executable_regions {
            let limit = *address + info.size;
            let build_id_string_index = st.intern_build_id(&info.build_id);
            builder.add_mapping(*address..limit, info.file_offset, build_id_string_index)?;
        }
        let (mappings, resolver) = builder.build();
        pprof.mapping = mappings;
        resolver
    };

    // Fill the Locations with all the program addresses referenced in the snapshot and store their
    // assigned IDs.
    let mut address_to_location_id = HashMap::new();
    for info in snapshot.allocations.values() {
        for address in &info.stack_trace.program_addresses {
            let next_id = address_to_location_id.len() as u64 + 1;
            if let Entry::Vacant(e) = address_to_location_id.entry(*address) {
                e.insert(next_id);
                pprof.location.push(pprof::Location {
                    id: next_id,
                    mapping_id: module_map.resolve(*address),
                    address: *address,
                    ..Default::default()
                });
            }
        }
    }

    // Helper function that translates program addresses to location IDs.
    let addresses_to_location_ids = |program_addresses: &[u64]| -> Vec<u64> {
        program_addresses.iter().map(|addr| address_to_location_id[addr]).collect()
    };

    // Fill the Samples.
    if with_tags {
        for (address, info) in &snapshot.allocations {
            // Cast into a pprof-friendly type.
            let size = info.size as i64;

            let location_ids = addresses_to_location_ids(&info.stack_trace.program_addresses);
            pprof.sample.push(pprof::Sample {
                location_id: location_ids,
                value: vec![1, size],
                label: vec![
                    pprof::Label {
                        key: st.intern("address"),
                        str: st.intern(&format!("0x{:x}", address)),
                        ..Default::default()
                    },
                    pprof::Label {
                        key: st.intern("bytes"),
                        num: size,
                        num_unit: st.intern("bytes"),
                        ..Default::default()
                    },
                    pprof::Label {
                        key: st.intern("timestamp"),
                        num: info.timestamp,
                        num_unit: st.intern("nanoseconds"),
                        ..Default::default()
                    },
                    pprof::Label {
                        key: st.intern("thread"),
                        str: st.intern(&format!(
                            "{}[{}]",
                            info.thread_info.name, info.thread_info.koid
                        )),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            });
        }
    } else {
        // Group allocations with the same stack trace (to make the resulting profile smaller).
        let grouped_allocations =
            snapshot.allocations.values().into_group_map_by(|allocation_info| {
                addresses_to_location_ids(&allocation_info.stack_trace.program_addresses)
            });

        for (location_ids, allocations_info) in grouped_allocations {
            // Compute totals and cast into pprof-friendly types.
            let size = allocations_info.iter().fold(0, |sum, elem| sum + elem.size) as i64;
            let count = allocations_info.len() as i64;

            pprof.sample.push(pprof::Sample {
                location_id: location_ids,
                value: vec![count, size],
                ..Default::default()
            });
        }
    }

    pprof.string_table = st.build();
    Ok(pprof)
}

pub fn export_to_pprof(
    snapshot: &Snapshot,
    dest: &mut std::fs::File,
    with_tags: bool,
) -> Result<()> {
    let buf = build_profile(snapshot, with_tags)?.encode_to_vec();
    dest.write_all(&buf)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use heapdump_snapshot::{Allocation, ExecutableRegion, StackTrace, ThreadInfo};
    use itertools::MinMaxResult::MinMax;
    use maplit::hashmap;
    use std::io::{Read, Seek};
    use std::rc::Rc;
    use test_case::test_case;

    // Placeholder mappings for the fake profile:
    const MAP_1_ADDRESS: u64 = 0x2000;
    const MAP_1_SIZE: u64 = 0x1000;
    const MAP_1_FILE_OFFSET: u64 = 0x1000;
    const MAP_1_BUILD_ID: &str = "112233441122334411223344";
    const MAP_2_ADDRESS: u64 = 0x8000;
    const MAP_2_SIZE: u64 = 0x2000;
    const MAP_2_FILE_OFFSET: u64 = 0;
    const MAP_2_BUILD_ID: &str = "556677885566778855667788";

    // Placeholder code locations (program addresses) for the fake profile:
    const LOC_1_ADDRESS: u64 = 0x2500; // within mapping 1
    const LOC_2_ADDRESS: u64 = 0x8900; // within mapping 2
    const LOC_3_ADDRESS: u64 = 0x9100; // within mapping 2
    const LOC_4_ADDRESS: u64 = 0; // outside of any known mapping
    const LOC_5_ADDRESS: u64 = 0x5123; // outside of any known mapping

    // Placeholder stack traces for the fake profile:
    const STACK_TRACE_A: &[u64] = &[LOC_1_ADDRESS, LOC_2_ADDRESS, LOC_3_ADDRESS];
    const STACK_TRACE_B: &[u64] = &[LOC_1_ADDRESS, LOC_4_ADDRESS, LOC_5_ADDRESS];

    // Placeholder allocations for the fake profile:
    const ALLOC_1_ADDRESS: u64 = 0x611000;
    const ALLOC_1_SIZE: i64 = 0x1800;
    const ALLOC_1_THREAD_KOID: u64 = 1234;
    const ALLOC_1_THREAD_NAME: &str = "thread-1";
    const ALLOC_1_TIMESTAMP: i64 = 8777777777778;
    const ALLOC_2_ADDRESS: u64 = 0x624000;
    const ALLOC_2_SIZE: i64 = 0x30;
    const ALLOC_2_THREAD_KOID: u64 = 5678;
    const ALLOC_2_THREAD_NAME: &str = "thread-2";
    const ALLOC_2_TIMESTAMP: i64 = 9333333333333;
    const ALLOC_3_ADDRESS: u64 = 0x756000;
    const ALLOC_3_SIZE: i64 = 0xC000;
    const ALLOC_3_THREAD_KOID: u64 = 9999;
    const ALLOC_3_THREAD_NAME: &str = "thread-3";
    const ALLOC_3_TIMESTAMP: i64 = 9876543211111;

    fn generate_fake_snapshot() -> Snapshot {
        let stack_trace_a = Rc::new(StackTrace { program_addresses: STACK_TRACE_A.to_vec() });
        let stack_trace_b = Rc::new(StackTrace { program_addresses: STACK_TRACE_B.to_vec() });

        Snapshot {
            allocations: hashmap![
                ALLOC_1_ADDRESS => Allocation {
                    size: ALLOC_1_SIZE.try_into().unwrap(),
                    thread_info: Rc::new(ThreadInfo {
                        koid: ALLOC_1_THREAD_KOID,
                        name: ALLOC_1_THREAD_NAME.to_string(),
                    }),
                    stack_trace: stack_trace_a.clone(),
                    timestamp: ALLOC_1_TIMESTAMP,
                    contents: None,
                },
                ALLOC_2_ADDRESS => Allocation {
                    size: ALLOC_2_SIZE.try_into().unwrap(),
                    thread_info: Rc::new(ThreadInfo {
                        koid: ALLOC_2_THREAD_KOID,
                        name: ALLOC_2_THREAD_NAME.to_string(),
                    }),
                    stack_trace: stack_trace_b.clone(),
                    timestamp: ALLOC_2_TIMESTAMP,
                    contents: None,
                },
                ALLOC_3_ADDRESS => Allocation {
                    size: ALLOC_3_SIZE.try_into().unwrap(),
                    thread_info: Rc::new(ThreadInfo {
                        koid: ALLOC_3_THREAD_KOID,
                        name: ALLOC_3_THREAD_NAME.to_string(),
                    }),
                    stack_trace: stack_trace_b.clone(),
                    timestamp: ALLOC_3_TIMESTAMP,
                    contents: None,
                },
            ],
            executable_regions: hashmap![
                MAP_1_ADDRESS => ExecutableRegion {
                    size: MAP_1_SIZE,
                    file_offset: MAP_1_FILE_OFFSET,
                    build_id: hex::decode(MAP_1_BUILD_ID).unwrap(),
                },
                MAP_2_ADDRESS => ExecutableRegion {
                    size: MAP_2_SIZE,
                    file_offset: MAP_2_FILE_OFFSET,
                    build_id: hex::decode(MAP_2_BUILD_ID).unwrap(),
                },
            ],
        }
    }

    fn assert_profile_matches_fake_snapshot(profile: &pprof::Profile, with_tags: bool) {
        // Helper function to access the string table.
        let st = |index: i64| profile.string_table[usize::try_from(index).unwrap()].as_str();

        // Helper function to resolce location IDs.
        let loc = |location_id: u64| profile.location.iter().find(|e| e.id == location_id).unwrap();

        // Verify the string table.
        assert_eq!(st(0), "", "The first entry in the string table should always be empty");

        // Verify that samples' data format.
        assert_eq!(profile.sample_type.len(), 2);
        assert_eq!(st(profile.sample_type[0].r#type), "objects");
        assert_eq!(st(profile.sample_type[0].unit), "count");
        assert_eq!(st(profile.sample_type[1].r#type), "allocated");
        assert_eq!(st(profile.sample_type[1].unit), "bytes");

        if with_tags {
            // Verify the tags' labels.
            for sample in &profile.sample {
                assert_eq!(sample.value.len(), profile.sample_type.len());
                assert_eq!(sample.label.len(), 4);
                assert_eq!(st(sample.label[0].key), "address");
                assert_eq!(st(sample.label[1].key), "bytes");
                assert_eq!(st(sample.label[1].num_unit), "bytes");
                assert_eq!(st(sample.label[2].key), "timestamp");
                assert_eq!(st(sample.label[2].num_unit), "nanoseconds");
                assert_eq!(st(sample.label[3].key), "thread");
            }

            // Identify the three allocations from their sizes (which are unique in our sample snapshot)
            // and verify them.
            assert_eq!(profile.sample.len(), 3);
            let allocation1 =
                profile.sample.iter().find(|s| s.label[1].num == ALLOC_1_SIZE).unwrap();
            assert_eq!(allocation1.value[0], 1);
            assert_eq!(allocation1.value[1], ALLOC_1_SIZE);
            assert_eq!(st(allocation1.label[0].str), format!("0x{:x}", ALLOC_1_ADDRESS));
            assert_eq!(allocation1.location_id.len(), STACK_TRACE_A.len());
            assert_eq!(loc(allocation1.location_id[0]).address, STACK_TRACE_A[0]);
            assert_eq!(loc(allocation1.location_id[1]).address, STACK_TRACE_A[1]);
            assert_eq!(loc(allocation1.location_id[2]).address, STACK_TRACE_A[2]);
            assert_eq!(allocation1.label[2].num, ALLOC_1_TIMESTAMP);
            assert_eq!(
                st(allocation1.label[3].str),
                format!("{}[{}]", ALLOC_1_THREAD_NAME, ALLOC_1_THREAD_KOID)
            );
            let allocation2 =
                profile.sample.iter().find(|s| s.label[1].num == ALLOC_2_SIZE).unwrap();
            assert_eq!(allocation2.value[0], 1);
            assert_eq!(allocation2.value[1], ALLOC_2_SIZE);
            assert_eq!(st(allocation2.label[0].str), format!("0x{:x}", ALLOC_2_ADDRESS));
            assert_eq!(allocation2.location_id.len(), STACK_TRACE_B.len());
            assert_eq!(loc(allocation2.location_id[0]).address, STACK_TRACE_B[0]);
            assert_eq!(loc(allocation2.location_id[1]).address, STACK_TRACE_B[1]);
            assert_eq!(allocation2.label[2].num, ALLOC_2_TIMESTAMP);
            assert_eq!(
                st(allocation2.label[3].str),
                format!("{}[{}]", ALLOC_2_THREAD_NAME, ALLOC_2_THREAD_KOID)
            );
            let allocation3 =
                profile.sample.iter().find(|s| s.label[1].num == ALLOC_3_SIZE).unwrap();
            assert_eq!(allocation3.value[0], 1);
            assert_eq!(allocation3.value[1], ALLOC_3_SIZE);
            assert_eq!(st(allocation3.label[0].str), format!("0x{:x}", ALLOC_3_ADDRESS));
            assert_eq!(allocation3.location_id.len(), STACK_TRACE_B.len());
            assert_eq!(loc(allocation3.location_id[0]).address, STACK_TRACE_B[0]);
            assert_eq!(loc(allocation3.location_id[1]).address, STACK_TRACE_B[1]);
            assert_eq!(allocation3.label[2].num, ALLOC_3_TIMESTAMP);
            assert_eq!(
                st(allocation3.label[3].str),
                format!("{}[{}]", ALLOC_3_THREAD_NAME, ALLOC_3_THREAD_KOID)
            );
        } else {
            // Verify that the samples were aggregated by stack trace correctly.
            assert_eq!(profile.sample.len(), 2);
            let MinMax(group1, group2) = profile.sample.iter().minmax_by_key(|e| e.value[0]) else {
                unreachable!();
            };
            assert_eq!(group1.value[0], 1); // i.e. allocation1
            assert_eq!(group1.value[1], ALLOC_1_SIZE);
            assert_eq!(group1.location_id.len(), STACK_TRACE_A.len());
            assert_eq!(loc(group1.location_id[0]).address, STACK_TRACE_A[0]);
            assert_eq!(loc(group1.location_id[1]).address, STACK_TRACE_A[1]);
            assert_eq!(loc(group1.location_id[2]).address, STACK_TRACE_A[2]);
            assert_eq!(group2.value[0], 2); // i.e. allocation2 and allocation3
            assert_eq!(group2.value[1], ALLOC_2_SIZE + ALLOC_3_SIZE);
            assert_eq!(group2.location_id.len(), STACK_TRACE_B.len());
            assert_eq!(loc(group2.location_id[0]).address, STACK_TRACE_B[0]);
            assert_eq!(loc(group2.location_id[1]).address, STACK_TRACE_B[1]);
        }

        // Identify the mappings from their addresses and verify them.
        assert_eq!(profile.mapping.len(), 2);
        assert_eq!(profile.mapping.iter().filter(|m| m.id == 0).next(), None, "ID 0 is reserved");
        let mapping1 = profile.mapping.iter().find(|m| m.memory_start == MAP_1_ADDRESS).unwrap();
        assert_eq!(mapping1.memory_limit, MAP_1_ADDRESS + MAP_1_SIZE);
        assert_eq!(mapping1.file_offset, MAP_1_FILE_OFFSET);
        assert_eq!(st(mapping1.build_id), MAP_1_BUILD_ID);
        let mapping2 = profile.mapping.iter().find(|m| m.memory_start == MAP_2_ADDRESS).unwrap();
        assert_eq!(mapping2.memory_limit, MAP_2_ADDRESS + MAP_2_SIZE);
        assert_eq!(mapping2.file_offset, MAP_2_FILE_OFFSET);
        assert_eq!(st(mapping2.build_id), MAP_2_BUILD_ID);

        // Identify the locations from their addresses and verify them.
        assert_eq!(profile.location.len(), 5);
        assert_eq!(profile.location.iter().filter(|l| l.id == 0).next(), None, "ID 0 is reserved");
        let loc1 = profile.location.iter().find(|l| l.address == LOC_1_ADDRESS).unwrap();
        assert_eq!(loc1.mapping_id, mapping1.id, "LOC_1_ADDRESS belongs to mapping 1");
        let loc2 = profile.location.iter().find(|l| l.address == LOC_2_ADDRESS).unwrap();
        assert_eq!(loc2.mapping_id, mapping2.id, "LOC_2_ADDRESS belongs to mapping 2");
        let loc3 = profile.location.iter().find(|l| l.address == LOC_3_ADDRESS).unwrap();
        assert_eq!(loc3.mapping_id, mapping2.id, "LOC_3_ADDRESS belongs to mapping 2");
        let loc4 = profile.location.iter().find(|l| l.address == LOC_4_ADDRESS).unwrap();
        assert_eq!(loc4.mapping_id, 0, "LOC_4_ADDRESS does not belong to any mapping");
        let loc5 = profile.location.iter().find(|l| l.address == LOC_5_ADDRESS).unwrap();
        assert_eq!(loc5.mapping_id, 0, "LOC_5_ADDRESS does not belong to any mapping");
    }

    /// Verifies that the protobuf message generated by `build_profile` contains correct data.
    #[test_case(true ; "with tags")]
    #[test_case(false ; "aggregated")]
    fn test_build_profile(with_tags: bool) {
        let snapshot = generate_fake_snapshot();
        let profile = build_profile(&snapshot, with_tags).unwrap();
        assert_profile_matches_fake_snapshot(&profile, with_tags);
    }

    /// Verifies that the file written by `export_to_pprof` can be read back.
    #[test_case(true ; "with tags")]
    #[test_case(false ; "aggregated")]
    fn test_export_to_pprof(with_tags: bool) {
        // Create a temporary file.
        let mut tempfile = tempfile::tempfile().unwrap();

        // Write a snapshot to it.
        let snapshot = generate_fake_snapshot();
        export_to_pprof(&snapshot, &mut tempfile, with_tags).unwrap();

        // Read it back.
        let mut buf = Vec::new();
        tempfile.rewind().unwrap();
        tempfile.read_to_end(&mut buf).unwrap();

        // Verify that it can be decoded correctly.
        let profile = pprof::Profile::decode(&buf[..]).unwrap();
        assert_profile_matches_fake_snapshot(&profile, with_tags);
    }
}
