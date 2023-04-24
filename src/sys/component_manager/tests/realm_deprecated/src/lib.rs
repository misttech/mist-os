// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_sys2::RealmQueryMarker;
use fuchsia_component::client::*;

#[fuchsia::test]
pub async fn query_get_instance_directories() {
    let query = connect_to_protocol::<RealmQueryMarker>().unwrap();

    let resolved_dirs = query.get_instance_directories("./").await.unwrap().unwrap().unwrap();

    let ns_entries = resolved_dirs.ns_entries;
    assert_eq!(ns_entries.len(), 2);

    for entry in ns_entries {
        let path = entry.path.unwrap();
        let dir = entry.directory.unwrap().into_proxy().unwrap();
        match path.as_str() {
            "/svc" => {
                let entries = fuchsia_fs::directory::readdir(&dir).await.unwrap();
                assert_eq!(
                    entries,
                    vec![
                        fuchsia_fs::directory::DirEntry {
                            name: "fidl.examples.routing.echo.Echo".to_string(),
                            kind: fuchsia_fs::directory::DirentKind::Unknown,
                        },
                        fuchsia_fs::directory::DirEntry {
                            name: "fuchsia.logger.LogSink".to_string(),
                            kind: fuchsia_fs::directory::DirentKind::Unknown,
                        },
                        fuchsia_fs::directory::DirEntry {
                            name: "fuchsia.sys2.RealmExplorer".to_string(),
                            kind: fuchsia_fs::directory::DirentKind::Unknown,
                        },
                        fuchsia_fs::directory::DirEntry {
                            name: "fuchsia.sys2.RealmQuery".to_string(),
                            kind: fuchsia_fs::directory::DirentKind::Unknown,
                        },
                    ]
                );
            }
            "/pkg" => {}
            path => panic!("unexpected directory: {}", path),
        }
    }

    let pkg_dir = resolved_dirs.pkg_dir.unwrap();
    let pkg_dir = pkg_dir.into_proxy().unwrap();
    let entries = fuchsia_fs::directory::readdir(&pkg_dir).await.unwrap();
    assert_eq!(
        entries,
        vec![
            fuchsia_fs::directory::DirEntry {
                name: "bin".to_string(),
                kind: fuchsia_fs::directory::DirentKind::Directory,
            },
            fuchsia_fs::directory::DirEntry {
                name: "data".to_string(),
                kind: fuchsia_fs::directory::DirentKind::Directory,
            },
            fuchsia_fs::directory::DirEntry {
                name: "lib".to_string(),
                kind: fuchsia_fs::directory::DirentKind::Directory,
            },
            fuchsia_fs::directory::DirEntry {
                name: "meta".to_string(),
                kind: fuchsia_fs::directory::DirentKind::Directory,
            }
        ]
    );

    let exposed_dir = resolved_dirs.exposed_dir.into_proxy().unwrap();
    let entries = fuchsia_fs::directory::readdir(&exposed_dir).await.unwrap();
    assert_eq!(
        entries,
        vec![fuchsia_fs::directory::DirEntry {
            name: "fuchsia.test.Suite".to_string(),
            kind: fuchsia_fs::directory::DirentKind::Unknown,
        },]
    );
}
