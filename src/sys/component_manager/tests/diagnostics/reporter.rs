// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    diagnostics_reader::{
        assert_data_tree, AnyProperty, ArchiveReader, DiagnosticsHierarchy, Inspect,
    },
    fidl::endpoints::create_proxy,
    fidl_fuchsia_io as fio, fidl_fuchsia_sys2 as fsys, fuchsia_fs,
};

async fn get_job_koid(moniker: &str, realm_query: &fsys::RealmQueryProxy) -> u64 {
    let (file_proxy, server_end) = create_proxy::<fio::FileMarker>().unwrap();
    let server_end = server_end.into_channel().into();
    realm_query
        .open(
            moniker,
            fsys::OpenDirType::RuntimeDir,
            fio::OpenFlags::RIGHT_READABLE,
            fio::ModeType::empty(),
            "elf/job_id",
            server_end,
        )
        .await
        .unwrap()
        .unwrap();
    let res = fuchsia_fs::file::read_to_string(&file_proxy).await;
    let contents = res.expect("Unable to read file.");
    contents.parse::<u64>().unwrap()
}

fn get_data(
    hierarchy: &DiagnosticsHierarchy,
    moniker: &str,
    task: &str,
) -> (Vec<i64>, Vec<i64>, Vec<i64>) {
    let path = ["stats", "measurements", "components", moniker, task];
    let node = hierarchy.get_child_by_path(&path).expect("found stats node");
    let cpu_times = node
        .get_property("cpu_times")
        .expect("found cpu")
        .int_array()
        .expect("cpu are ints")
        .raw_values();
    let queue_times = node
        .get_property("queue_times")
        .expect("found queue")
        .int_array()
        .expect("queue are ints")
        .raw_values();
    let timestamps = node
        .get_property("timestamps")
        .expect("found timestamps")
        .int_array()
        .expect("timestamps are ints")
        .raw_values();
    (timestamps.into_owned(), cpu_times.into_owned(), queue_times.into_owned())
}

fn assert_component_data(hierarchy: &DiagnosticsHierarchy, moniker: &str, koid: u64) {
    let (timestamps, cpu_times, queue_times) = get_data(hierarchy, moniker, &koid.to_string());
    assert_eq!(timestamps.len(), 2);
    assert_eq!(cpu_times.len(), 2);
    assert_eq!(queue_times.len(), 2);
    assert_eq!(cpu_times[0], 0);
    assert_eq!(queue_times[0], 0);
}

#[fuchsia::main]
async fn main() {
    let data = ArchiveReader::new()
        .add_selector("<component_manager>:root")
        .snapshot::<Inspect>()
        .await
        .expect("got inspect data");

    let realm_query =
        fuchsia_component::client::connect_to_protocol::<fsys::RealmQueryMarker>().unwrap();

    let archivist_job_koid = get_job_koid("./archivist", &realm_query).await;
    let reporter_job_koid = get_job_koid("./reporter", &realm_query).await;

    assert_eq!(data.len(), 1, "expected 1 match: {:?}", data);
    let hierarchy = data[0].payload.as_ref().unwrap();
    assert_data_tree!(hierarchy, root: {
        "fuchsia.inspect.Health": {
            start_timestamp_nanos: AnyProperty,
            status: "OK"
        },
        early_start_times: {
            "0": {
                moniker: ".",
                time: AnyProperty,
            },
            "1": {
                moniker: "root",
                time: AnyProperty,
            },
            "2": {
                moniker: "root/reporter",
                time: AnyProperty,
            },
            "3": {
                moniker: "root/archivist",
                time: AnyProperty,
            },
        },
        stats: contains {
            measurements: {
                component_count: 3u64,
                task_count: 3u64,
                "fuchsia.inspect.Stats": {
                    current_size: AnyProperty,
                    maximum_size: AnyProperty,
                    total_dynamic_children: AnyProperty,
                    allocated_blocks: AnyProperty,
                    deallocated_blocks: AnyProperty,
                    failed_allocations: 0u64,
                },
                components: {
                    "<component_manager>": contains {},
                    "root/archivist": {
                        archivist_job_koid.to_string() => {
                            timestamps: AnyProperty,
                            cpu_times: AnyProperty,
                            queue_times: AnyProperty,
                        }
                    },
                    "root/reporter": {
                        reporter_job_koid.to_string() => {
                            timestamps: AnyProperty,
                            cpu_times: AnyProperty,
                            queue_times: AnyProperty,
                        }
                    },
                }
            },
        },
        "fuchsia.inspect.Stats": {
            current_size: AnyProperty,
            maximum_size: AnyProperty,
            total_dynamic_children: AnyProperty,
            allocated_blocks: AnyProperty,
            deallocated_blocks: AnyProperty,
            failed_allocations: 0u64,
        }
    });

    assert_component_data(hierarchy, "root/archivist", archivist_job_koid);
    assert_component_data(hierarchy, "root/reporter", reporter_job_koid);
}
