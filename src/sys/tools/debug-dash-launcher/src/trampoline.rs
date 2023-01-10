// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl::endpoints::create_proxy;
use fidl::HandleBased;
use fidl_fuchsia_dash::LauncherError;
use fidl_fuchsia_io as fio;
use fidl_fuchsia_kernel as fkernel;
use fidl_fuchsia_pkg as fpkg;
use fuchsia_component::client::connect_to_protocol;
use fuchsia_url::AbsolutePackageUrl;
use fuchsia_zircon as zx;
use std::sync::Arc;
use vfs::directory::entry::DirectoryEntry;
use vfs::directory::helper::DirectlyMutable;
use vfs::directory::mutable::connection::io1::MutableConnection;
use vfs::directory::simple::Simple;
use vfs::file::vmo::asynchronous as vmo;

// The location of the added trampolines. The path will be of the form:
// `/.dash/tools/<package-name>/<trampoline-name>`.
const BASE_TOOLS_PATH: &str = "/.dash/tools";

// For each package, PkgDir holds the URL and its resolved directory.
struct PkgDirs {
    pkg_url: AbsolutePackageUrl,
    dir: fio::DirectoryProxy,
}

async fn get_pkg_dirs(
    tool_urls: Vec<String>,
    resolver: fpkg::PackageResolverProxy,
) -> Result<Vec<PkgDirs>, LauncherError> {
    let mut dirs: Vec<PkgDirs> = vec![];
    for url in tool_urls {
        let pkg_url = url.parse::<AbsolutePackageUrl>().map_err(|_| LauncherError::BadUrl)?;
        let (dir, server) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
            .map_err(|_| LauncherError::Internal)?;
        let _subpackage_context =
            resolver.resolve(&url, server).await.map_err(|_| LauncherError::PackageResolver)?;
        dirs.push(PkgDirs { pkg_url, dir });
    }
    Ok(dirs)
}

// A Trampoline holds the resolve script contents and the name it will use.
struct Trampoline {
    contents: String,
    binary_name: String,
}

// PkgTrampolines contain the list of required trampolines for each package.
struct PkgTrampolines {
    pkg_name: String,
    pkg_trampolines: Vec<Trampoline>,
}

// For each package, create the trampoline specifications for each of its binaries.
// For the user's information, a directory entry will be created even if there are no
// binaries found.
async fn create_trampolines(pkg_dirs: &Vec<PkgDirs>) -> Result<Vec<PkgTrampolines>, LauncherError> {
    let mut all_trampolines: Vec<PkgTrampolines> = vec![];
    for pkg_dir in pkg_dirs {
        let mut pkg_trampolines: Vec<Trampoline> = vec![];

        // Read the package binaries.
        let bin_dir = fuchsia_fs::directory::open_directory(
            &pkg_dir.dir,
            "bin",
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE,
        )
        .await
        .map_err(|_| LauncherError::ToolsBinaryRead)?;
        let entries =
            fuchsia_fs::directory::readdir(&bin_dir).await.map_err(|_| LauncherError::Internal)?;

        // Create the trampoline specifications.
        for entry in entries {
            if entry.kind == fio::DirentType::File {
                let contents = format!("#!resolve {}#bin/{}\n", &pkg_dir.pkg_url, entry.name);
                let binary_name = entry.name;
                pkg_trampolines.push(Trampoline { contents, binary_name });
            }
        }
        // Package names are used in the directory layout to allow binary names to repeat among
        // packages. However, the pkg_name uses only the URL's name(), dropping the host, variant,
        // and hash, if present. The result is that the paths shown to the user will be readable,
        // like `/.dash/tools/hello-world/hello_world_rust` and
        // `/.dash/tools/debug-dash-launcher/ls`. However, as only unique package names can be added
        // to a directory, this means that binaries cannot be added from packages differing only the
        // host, variant, or hash. The simple workaround is to add such packages in separate
        // invocations of `ffx component explore`.
        let pkg_name = pkg_dir.pkg_url.name().to_string();
        all_trampolines.push(PkgTrampolines { pkg_name, pkg_trampolines });
    }
    Ok(all_trampolines)
}

async fn create_vmex_resource() -> Result<zx::Resource, LauncherError> {
    let vmex_proxy = connect_to_protocol::<fkernel::VmexResourceMarker>()
        .map_err(|_| LauncherError::VmexResource)?;
    let vmex_resource = vmex_proxy.get().await.map_err(|_| LauncherError::VmexResource)?;
    Ok(zx::Resource::from(vmex_resource))
}

fn make_executable_vmo_file(
    resource: &zx::Resource,
    contents: String,
) -> Result<Arc<dyn DirectoryEntry>, LauncherError> {
    let vmo = zx::Vmo::create(contents.len() as u64).map_err(|_| LauncherError::Internal)?;
    vmo.write(contents.as_bytes(), 0).map_err(|_| LauncherError::Internal)?;

    // Make it into a VMO that can be loaded as an executable.
    let exec_vmo = vmo.replace_as_executable(&resource).map_err(|_| LauncherError::Internal)?;

    // Serve as a VMO file that accepts the read and executable Open rights.
    let init_vmo = move || {
        let dup = exec_vmo
            .duplicate_handle(zx::Rights::SAME_RIGHTS)
            .expect("failed to duplicate VMO in make_vfs().");
        futures::future::ok(dup)
    };
    let read_exec_vmo = vmo::read_exec(init_vmo);
    Ok(read_exec_vmo)
}

// Return a proxy to the given directory, first opening it as executable.
fn directory_to_proxy(
    dir: Arc<Simple<MutableConnection>>,
) -> Result<fio::DirectoryProxy, LauncherError> {
    let (client, server) =
        create_proxy::<fio::DirectoryMarker>().map_err(|_| LauncherError::Internal)?;
    let scope = vfs::execution_scope::ExecutionScope::new();
    dir.open(
        scope.clone(),
        fio::OpenFlags::RIGHT_READABLE
            | fio::OpenFlags::RIGHT_WRITABLE
            | fio::OpenFlags::RIGHT_EXECUTABLE,
        0,
        vfs::path::Path::dot(),
        server.into_channel().into(),
    );
    Ok(client)
}

// Given a list of package trampoline specifications, create the executable files and add them into
// a new directory. Give each package its own subdirectory based on the package name, preventing
// binary name collisions. Binary names can repeat if they are found in different packages. However,
// only the package name is considered, so packages cannot differ only in host, variant, or hash.
//
// The package directories are added in the order they are given on the command line. The enclosing
// directory is prepended to the path environment variable. The resulting preference order for
// same-name binaries is therefore first packages in load order, then built-ins. To avoid shadowing,
// use the complete path to the binary: $ .dash/tools/<pkg>/<binary>
//
// Return the VFS directory containing the executables and the associated path environment variable.
async fn make_trampoline_vfs(
    pkg_trampolines: Vec<PkgTrampolines>,
) -> Result<(Option<fio::DirectoryProxy>, Option<String>), LauncherError> {
    if pkg_trampolines.is_empty() {
        return Ok((None, None));
    }
    let resource = create_vmex_resource().await?;
    let tools_dir = vfs::mut_pseudo_directory! {};
    let mut path = String::new();
    for pkg_trampoline in pkg_trampolines {
        let pkg_name = pkg_trampoline.pkg_name;
        if !path.is_empty() {
            path.push(':');
        }
        path.push_str(&format!("{}/{}", BASE_TOOLS_PATH, &pkg_name));
        let pkg_dir = vfs::mut_pseudo_directory! {};

        for trampoline in pkg_trampoline.pkg_trampolines {
            let read_exec_vmo = make_executable_vmo_file(&resource, trampoline.contents)?;
            pkg_dir
                .add_entry(&trampoline.binary_name, read_exec_vmo)
                .map_err(|_| LauncherError::Internal)?;
        }
        // This line will fail if the user tries to add two packages with the the same package name.
        tools_dir.add_entry(&pkg_name, pkg_dir).map_err(|_| LauncherError::NonUniquePackageName)?;
    }
    // Return it as an executable directory.
    let dir = directory_to_proxy(tools_dir)?;
    Ok((Some(dir), Some(path)))
}

// Given the URLs of some packages, return a directory containing their binaries as trampolines.
pub async fn create_trampolines_from_packages(
    pkg_urls: Vec<String>,
) -> Result<(Option<fio::DirectoryProxy>, Option<String>), LauncherError> {
    if pkg_urls.is_empty() {
        return Ok((None, None));
    }

    let resolver = connect_to_protocol::<fpkg::PackageResolverMarker>()
        .map_err(|_| LauncherError::PackageResolver)?;

    let pkg_dirs = get_pkg_dirs(pkg_urls, resolver).await?;
    let trampolines = create_trampolines(&pkg_dirs).await?;
    make_trampoline_vfs(trampolines).await
}

// Create a PATH environment variable from the tools_path if present.
pub fn create_env_path(tools_path: Option<String>) -> String {
    let mut path_envvar = "".to_string();
    if let Some(tp) = tools_path {
        path_envvar.push_str("PATH=");
        path_envvar.push_str(&tp.to_string());
    }
    path_envvar
}

#[cfg(test)]
mod tests {
    use super::*;
    use fidl::endpoints::create_proxy;
    use vfs::directory::entry::DirectoryEntry;
    use vfs::execution_scope::ExecutionScope;
    use vfs::file::vmo::read_only_static;
    use vfs::path::Path;

    async fn open_directory(server: Arc<dyn DirectoryEntry>) -> fio::DirectoryProxy {
        let (proxy, server_end) =
            create_proxy::<fio::DirectoryMarker>().expect("Failed to create connection endpoints");

        let flags = fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE;
        server.open(ExecutionScope::new(), flags, 0, Path::dot(), server_end.into_channel().into());
        proxy
    }

    #[fuchsia::test]
    async fn create_trampolines_test() {
        async fn create_test_directory_proxies() -> Vec<PkgDirs> {
            let root = vfs::pseudo_directory! {
                "Nasa" => vfs::pseudo_directory! {
                    "bin" => vfs::pseudo_directory! {
                        "go2moon_v1969" => read_only_static(b"Apollo"),
                        "go2moon_v2024" => read_only_static(b"Artemis"),
                    },
                },
                "SpaceX" => vfs::pseudo_directory! {
                    "bin" => vfs::pseudo_directory! {
                        "go2orbit" => read_only_static(b"Falcon 9"),
                        "go2mars" => read_only_static(b"Starship"),
                        "bogus_dir" => vfs::pseudo_directory! {"bogus_file" => read_only_static(b"bogus_content"),}
                    },
                },
                "BlueOrigin" => vfs::pseudo_directory! {
                    "bin" => vfs::pseudo_directory! {},
                },
            };
            vec![
                PkgDirs {
                    pkg_url: "fuchsia-pkg://earth.org/nasa_pkg"
                        .parse::<AbsolutePackageUrl>()
                        .unwrap(),
                    dir: open_directory(
                        root.get_entry("Nasa").expect("error obtaining directory entry"),
                    )
                    .await,
                },
                PkgDirs {
                    pkg_url: "fuchsia-pkg://earth.org/spacex_pkg"
                        .parse::<AbsolutePackageUrl>()
                        .unwrap(),
                    dir: open_directory(
                        root.get_entry("SpaceX").expect("error obtaining directory entry"),
                    )
                    .await,
                },
                PkgDirs {
                    pkg_url: "fuchsia-pkg://earth.org/blueorigin_pkg"
                        .parse::<AbsolutePackageUrl>()
                        .unwrap(),
                    dir: open_directory(
                        root.get_entry("BlueOrigin").expect("error obtaining directory entry"),
                    )
                    .await,
                },
            ]
        }

        let pkg_dirs = create_test_directory_proxies().await;
        let r = create_trampolines(&pkg_dirs).await.unwrap();
        assert_eq!(r.len(), 3); // None for "blueorigin_pkg", which had no binaries.
        let r0 = &r[0].pkg_trampolines;
        assert_eq!(r0.len(), 2);
        assert_eq!(
            r0[0].contents,
            "#!resolve fuchsia-pkg://earth.org/nasa_pkg#bin/go2moon_v1969\n".to_string()
        );
        assert_eq!(r0[0].binary_name, "go2moon_v1969".to_string());
        assert_eq!(
            r0[1].contents,
            "#!resolve fuchsia-pkg://earth.org/nasa_pkg#bin/go2moon_v2024\n".to_string()
        );
        assert_eq!(r0[1].binary_name, "go2moon_v2024".to_string());

        let r1 = &r[1].pkg_trampolines;
        assert_eq!(r1.len(), 2);
        assert_eq!(
            r1[0].contents,
            "#!resolve fuchsia-pkg://earth.org/spacex_pkg#bin/go2mars\n".to_string()
        );
        assert_eq!(r1[0].binary_name, "go2mars".to_string());
        assert_eq!(
            r1[1].contents,
            "#!resolve fuchsia-pkg://earth.org/spacex_pkg#bin/go2orbit\n".to_string()
        );
        assert_eq!(r1[1].binary_name, "go2orbit".to_string());

        assert_eq!(r[2].pkg_trampolines.len(), 0);
    }

    #[fuchsia::test]
    async fn create_env_path_test() {
        assert_eq!(create_env_path(Some("bar".to_string())), "PATH=bar");
        assert_eq!(create_env_path(Some("".to_string())), "PATH=");
        assert_eq!(create_env_path(Some(" ".to_string())), "PATH= ");
        assert_eq!(create_env_path(None), "");
    }
}
