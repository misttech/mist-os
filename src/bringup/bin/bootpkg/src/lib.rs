// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod args;

use crate::args::{Args, ShowCommand, SubCommand};
use anyhow::{format_err, Result};
use fidl_fuchsia_io as fio;
use fuchsia_pkg::MetaContents;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Cursor, Read};

pub fn bootpkg(boot_dir: File, args: Args) -> Result<()> {
    match args.command {
        SubCommand::List(_) => list(&boot_dir),
        SubCommand::Show(ShowCommand { package_name }) => show(&boot_dir, &package_name),
    }
}

type PackageList = HashMap<String, fuchsia_merkle::Hash>;

fn read_file(boot_dir: &File, path: &str) -> Result<Vec<u8>> {
    let mut file = fdio::open_fd_at(boot_dir, path, fio::Flags::PERM_READ)?;
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer)?;
    Ok(buffer)
}

fn get_package_list(boot_dir: &File) -> Result<PackageList> {
    let contents = read_file(boot_dir, "data/bootfs_packages")?;
    Ok(MetaContents::deserialize(&contents[..])?.into_contents())
}

fn list(boot_dir: &File) -> Result<()> {
    let package_list = get_package_list(boot_dir)?;
    let mut package_list = package_list.iter().map(|(name, _)| name).collect::<Vec<_>>();
    package_list.sort();
    for name in package_list {
        println!("{name}");
    }

    Ok(())
}

fn show(boot_dir: &File, package_name: &str) -> Result<()> {
    let package_list = get_package_list(boot_dir)?;
    let Some(merkle) = package_list.get(package_name) else {
        return Err(format_err!("package '{package_name}' not found in package list"));
    };
    let meta_far = read_file(boot_dir, &format!("blob/{merkle}"))?;

    let mut reader = fuchsia_archive::Reader::new(Cursor::new(meta_far))?;
    let reader_list = reader.list();
    let mut meta_files = HashSet::with_capacity(reader_list.len());
    for entry in reader_list {
        let path = String::from_utf8(entry.path().to_vec())?;
        if path.starts_with("meta/") {
            for (i, _) in path.match_indices('/').skip(1) {
                if meta_files.contains(&path[..i]) {
                    return Err(format_err!("Colliding entries in meta archive"));
                }
            }
            meta_files.insert(path);
        }
    }

    let meta_contents_bytes = reader.read_file(b"meta/contents")?;
    let non_meta_files = MetaContents::deserialize(&meta_contents_bytes[..])?.into_contents();

    let mut files = meta_files
        .iter()
        .chain(non_meta_files.iter().map(|(name, _)| name).filter(|name| *name != "/0"))
        .collect::<Vec<_>>();
    files.sort();

    for name in files {
        println!("{name}");
    }

    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    use fidl::endpoints::{create_endpoints, ServerEnd};
    use fuchsia_async as fasync;
    use fuchsia_merkle::Hash;
    use maplit::hashmap;
    use std::collections::BTreeMap;
    use std::io::Read;
    use std::str::FromStr;
    use vfs::directory::entry_container::Directory;
    use vfs::execution_scope::ExecutionScope;
    use vfs::file::vmo::read_only;
    use vfs::pseudo_directory;

    #[fasync::run_singlethreaded(test)]
    async fn list_test() {
        // prep boot directory.
        let contents = hashmap! {
            "foo".to_string() =>
                Hash::from_str("b21b34f8370687249a9cd9d4b306dee4c81f1f854f84de4626dc00c000c902fe").unwrap(),
            "bar".to_string() =>
                Hash::from_str("d0ff2aa87c938862d56fff76c9fe362240d2d51699506753e5840e05d41a3bf2").unwrap(),
            "baz".to_string() =>
                Hash::from_str("d0ff2aa87c938862d56fff76c9fe362240d2d51699506753e5840e05d41a3bf2").unwrap(),
        };
        let contents = MetaContents::from_map(contents).unwrap();
        let mut data = Vec::new();
        contents.serialize(&mut data).unwrap();

        let boot_dir = pseudo_directory! {
            "data" => pseudo_directory! {
                "bootfs_packages" => read_only(data),
            },
        };
        let (client, server) = create_endpoints::<fidl_fuchsia_io::DirectoryMarker>();
        let scope = ExecutionScope::new();
        boot_dir.open(
            scope,
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::DIRECTORY,
            vfs::path::Path::dot(),
            ServerEnd::new(server.into_channel()),
        );
        fasync::unblock(move || {
            let client: File = fdio::create_fd(client.into_channel().into()).unwrap().into();
            list(&client).unwrap();
        })
        .await;
    }

    #[fasync::run_singlethreaded(test)]
    async fn show_test() {
        // prep boot directory.
        let contents = hashmap! {
            "foo".to_string() =>
                Hash::from_str("b21b34f8370687249a9cd9d4b306dee4c81f1f854f84de4626dc00c000c902fe").unwrap(),
            "bar".to_string() =>
                Hash::from_str("d0ff2aa87c938862d56fff76c9fe362240d2d51699506753e5840e05d41a3bf2").unwrap(),
            "baz".to_string() =>
                Hash::from_str("d0ff2aa87c938862d56fff76c9fe362240d2d51699506753e5840e05d41a3bf2").unwrap(),
        };
        let contents = MetaContents::from_map(contents).unwrap();
        let mut data = Vec::new();
        contents.serialize(&mut data).unwrap();

        let meta_contents = data.clone();
        let mut path_content_map: BTreeMap<&str, (u64, Box<dyn Read>)> = BTreeMap::new();
        path_content_map
            .insert("meta/contents", (meta_contents.len() as u64, Box::new(&meta_contents[..])));
        let mut far_contents = Vec::new();
        fuchsia_archive::write(&mut far_contents, path_content_map).unwrap();

        let boot_dir = pseudo_directory! {
            "data" => pseudo_directory! {
                "bootfs_packages" => read_only(data),
            },
            "blob" => pseudo_directory! {
                "b21b34f8370687249a9cd9d4b306dee4c81f1f854f84de4626dc00c000c902fe" => read_only(far_contents),
            }
        };
        let (client, server) = create_endpoints::<fidl_fuchsia_io::DirectoryMarker>();
        let scope = ExecutionScope::new();
        boot_dir.open(
            scope,
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::DIRECTORY,
            vfs::path::Path::dot(),
            ServerEnd::new(server.into_channel()),
        );
        fasync::unblock(move || {
            let client: File = fdio::create_fd(client.into_channel().into()).unwrap().into();
            show(&client, "foo").unwrap();
        })
        .await;
    }
}
