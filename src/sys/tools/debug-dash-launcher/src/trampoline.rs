// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl::endpoints::create_proxy;
use fidl_fuchsia_dash::LauncherError;
use fidl_fuchsia_io as fio;
use fidl_fuchsia_kernel as fkernel;
use fidl_fuchsia_pkg as fpkg;
use fuchsia_component::client::connect_to_protocol;
use fuchsia_url::{AbsoluteComponentUrl, AbsolutePackageUrl};
use fuchsia_zircon as zx;
use indexmap::IndexMap;
use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use vfs::directory::entry::DirectoryEntry;
use vfs::directory::helper::DirectlyMutable;
use vfs::directory::mutable::connection::io1::MutableConnection;
use vfs::directory::simple::Simple;
use vfs::file::vmo;

// The location of the added trampolines. The path will be of the form:
// `/.dash/tools/<package-name>/<trampoline-name>`.
const BASE_TOOLS_PATH: &str = "/.dash/tools";

// For each package, PkgDir holds the URL and its resolved directory.
#[derive(Debug)] // Necessary for PkgDir to be used in assert_matches!().
struct PkgDir {
    pkg_url: AbsolutePackageUrl,
    dir: fio::DirectoryProxy,
    resource: Option<String>,
}

fn parse_url(url: &str) -> Result<(AbsolutePackageUrl, Option<String>), LauncherError> {
    if let Ok(comp_url) = url.parse::<AbsoluteComponentUrl>() {
        return Ok((comp_url.package_url().clone(), Some(comp_url.resource().to_string())));
    }
    Ok((url.parse::<AbsolutePackageUrl>().map_err(|_| LauncherError::BadUrl)?, None))
}

// For each of the given packages, resolve them and create a PkgDir with its URL and DirectoryProxy.
async fn get_pkg_dirs(
    tool_urls: Vec<String>,
    resolver: &fpkg::PackageResolverProxy,
) -> Result<Vec<PkgDir>, LauncherError> {
    let mut dirs: Vec<PkgDir> = vec![];
    for url in tool_urls {
        let (pkg_url, resource) = parse_url(&url)?;
        let (dir, server) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
            .map_err(|_| LauncherError::Internal)?;
        resolver
            .resolve(&pkg_url.to_string(), server)
            .await
            .map_err(|_| LauncherError::Internal)?
            .map_err(|_| LauncherError::PackageResolver)?;
        dirs.push(PkgDir { pkg_url, dir, resource });
    }
    Ok(dirs)
}

// A Trampoline holds the resolve script contents and the name that the used to run it.
#[derive(Clone)]
struct Trampoline {
    contents: String,
    binary_name: String,
}

// Using a BTreeSet will ensure that binaries are unique by name and are stored in insertion order.
// The Hash, Ord, PartialOrd, PartialEq, and Eq implementations enable Trampoline to be used in
// BTreeSet and are defined only on the `binary_name` field.
impl Hash for Trampoline {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.binary_name.hash(state);
    }
}
impl Ord for Trampoline {
    fn cmp(&self, other: &Self) -> Ordering {
        self.binary_name.cmp(&other.binary_name)
    }
}

impl PartialOrd for Trampoline {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Trampoline {
    fn eq(&self, other: &Self) -> bool {
        self.binary_name == other.binary_name
    }
}
impl Eq for Trampoline {}

// A package can have multiple binaries, so Trampolines contains the trampolines for each package.
struct Trampolines {
    // Use an IndexMap to maintain the keys in insertion order.
    map: IndexMap<String, BTreeSet<Trampoline>>,
}

impl Trampolines {
    fn new() -> Self {
        Trampolines { map: IndexMap::new() }
    }

    fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    // Insert the given set of trampolines into the map. The trampolines are merged in to common
    // package names. The trampoline names within packages are not repeated. Return a
    // NonUniqueBinaryName if an attempt is made to add a duplicate trampoline name within a
    // package.
    fn insert(
        &mut self,
        pkg_name: String,
        trampolines: BTreeSet<Trampoline>,
    ) -> Result<(), LauncherError> {
        match self.map.get_mut(&pkg_name) {
            None => {
                self.map.insert(pkg_name, trampolines);
            }
            Some(set) => {
                for t in &trampolines {
                    if !set.insert(t.clone()) {
                        return Err(LauncherError::NonUniqueBinaryName);
                    }
                }
            }
        }
        Ok(())
    }

    // For each of the entries in the map, create an executable VMO file in a directory named after
    // the package. Accumulate these into a tools directory, updating a path string as well. Return
    // the directory and the path.
    fn make_tools_dir(
        &self,
        resource: zx::Resource,
    ) -> Result<(Arc<Simple<MutableConnection>>, String), LauncherError> {
        let tools_dir = vfs::mut_pseudo_directory! {};
        let mut path = String::new();
        for (pkg_name, trampolines) in &self.map {
            if !path.is_empty() {
                path.push(':');
            }
            path.push_str(&format!("{}/{}", BASE_TOOLS_PATH, &pkg_name));
            let pkg_dir = vfs::mut_pseudo_directory! {};

            for trampoline in trampolines {
                let read_exec_vmo = make_executable_vmo_file(&resource, &trampoline.contents)?;
                pkg_dir
                    .add_entry(&trampoline.binary_name, read_exec_vmo)
                    .map_err(|_| LauncherError::Internal)?;
            }
            tools_dir.add_entry(pkg_name, pkg_dir).map_err(|_| LauncherError::Internal)?;
        }
        Ok((tools_dir, path))
    }
}

// For each package, create the trampoline specifications for each of its binaries.
// For the user's information, a directory entry will be created even if there are no
// binaries found.
async fn create_trampolines(pkg_dirs: &Vec<PkgDir>) -> Result<Trampolines, LauncherError> {
    let mut trampolines = Trampolines::new();
    for pkg_dir in pkg_dirs {
        match &pkg_dir.resource {
            Some(res) => {
                let contents = format!("#!resolve {}#{}\n", &pkg_dir.pkg_url, res);
                let binary_name =
                    res.split('/').next_back().ok_or_else(|| LauncherError::BadUrl)?.to_string();
                let pkg_name = pkg_dir.pkg_url.name().to_string();
                let mut set = BTreeSet::new();
                set.insert(Trampoline { contents, binary_name });
                trampolines.insert(pkg_name, set)?;
            }
            None => {
                // Read the package binaries.
                let bin_dir = fuchsia_fs::directory::open_directory(
                    &pkg_dir.dir,
                    "bin",
                    fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE,
                )
                .await
                .map_err(|_| LauncherError::ToolsBinaryRead)?;
                let entries = fuchsia_fs::directory::readdir(&bin_dir)
                    .await
                    .map_err(|_| LauncherError::Internal)?;

                // Create the trampoline specifications.
                let mut pkg_trampolines = BTreeSet::new();
                for entry in entries {
                    if entry.kind == fio::DirentType::File {
                        let contents =
                            format!("#!resolve {}#bin/{}\n", &pkg_dir.pkg_url, entry.name);
                        let binary_name = entry.name;
                        pkg_trampolines.insert(Trampoline { contents, binary_name });
                    }
                }
                // Package names are used in the directory layout to allow binary names to repeat
                // among packages. However, the pkg_name uses only the URL's name(), dropping the
                // host, variant, and hash, if present. The result is that the paths shown to the
                // user will be readable, like `/.dash/tools/hello-world/hello_world_rust` and
                // `/.dash/tools/debug-dash-launcher/ls`. However, as only unique package names can
                // be added to a directory, this means that binaries cannot be added from packages
                // differing only the host, variant, or hash. The simple workaround is to add such
                // packages in separate invocations of `ffx component explore`.
                let pkg_name = pkg_dir.pkg_url.name().to_string();
                trampolines.insert(pkg_name, pkg_trampolines)?;
            }
        }
    }
    Ok(trampolines)
}

async fn create_vmex_resource() -> Result<zx::Resource, LauncherError> {
    let vmex_proxy = connect_to_protocol::<fkernel::VmexResourceMarker>()
        .map_err(|_| LauncherError::VmexResource)?;
    let vmex_resource = vmex_proxy.get().await.map_err(|_| LauncherError::VmexResource)?;
    Ok(zx::Resource::from(vmex_resource))
}

fn make_executable_vmo_file(
    resource: &zx::Resource,
    contents: &str,
) -> Result<Arc<dyn DirectoryEntry>, LauncherError> {
    let vmo = zx::Vmo::create(contents.len() as u64).map_err(|_| LauncherError::Internal)?;
    vmo.write(contents.as_bytes(), 0).map_err(|_| LauncherError::Internal)?;

    // Make it into a VMO that can be loaded as an executable.
    let exec_vmo = vmo.replace_as_executable(&resource).map_err(|_| LauncherError::Internal)?;
    let exec_file = vmo::VmoFile::new(
        exec_vmo, /*readable*/ true, /*writable*/ false, /*executable*/ true,
    );
    Ok(exec_file)
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
    trampolines: Trampolines,
) -> Result<(Option<fio::DirectoryProxy>, Option<String>), LauncherError> {
    if trampolines.is_empty() {
        return Ok((None, None));
    }
    let resource = create_vmex_resource().await?;
    let (tools_dir, path) = trampolines.make_tools_dir(resource)?;
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

    let pkg_dirs = get_pkg_dirs(pkg_urls, &resolver).await?;
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
    use assert_matches::assert_matches;
    use fidl::endpoints::create_proxy_and_stream;
    use fidl_fuchsia_pkg as fpkg;
    use fio::OpenFlags;
    use fuchsia_async as fasync;
    use fuchsia_fs::directory::open_file;
    use fuchsia_fs::file::read_to_string;
    use futures::StreamExt;
    use std::fmt;
    use vfs::directory::immutable::connection::io1::ImmutableConnection;
    use vfs::execution_scope::ExecutionScope;
    use vfs::file::vmo::read_only;
    use vfs::path::Path;

    #[fuchsia::test]
    async fn parse_url_test() {
        assert_matches!(parse_url(""), Err(LauncherError::BadUrl));
        assert_matches!(parse_url("f"), Err(LauncherError::BadUrl));
        assert_matches!(parse_url("fuchsia-pk://h/n#foo"), Err(LauncherError::BadUrl));
        assert_matches!(parse_url("fuchsia-pkg#://h/n#"), Err(LauncherError::BadUrl));
        assert_matches!(parse_url("fuchsia-pkg#://h/n"), Err(LauncherError::BadUrl));
        assert_matches!(parse_url("fuchsia-pkg://h"), Err(LauncherError::BadUrl));
        assert_matches!(parse_url("fuchsia-pkg://h/#"), Err(LauncherError::BadUrl));
        assert_matches!(parse_url("fuchsia-pkg://📡/n#foo"), Err(LauncherError::BadUrl));

        let (abs_url, res) = parse_url("fuchsia-pkg://h/n").unwrap();
        assert_eq!(&abs_url.to_string(), "fuchsia-pkg://h/n");
        assert_eq!(res, None);

        // Multiple #'s allowed by fuchsia_url's parser!
        let (abs_url, res) = parse_url("fuchsia-pkg://h/n###foo").unwrap();
        assert_eq!(&abs_url.to_string(), "fuchsia-pkg://h/n");
        assert_eq!(res, Some("##foo".to_string()));

        let (abs_url, res) = parse_url("fuchsia-pkg://h/n#foo/bar").unwrap();
        assert_eq!(&abs_url.to_string(), "fuchsia-pkg://h/n");
        assert_eq!(res, Some("foo/bar".to_string()));

        let (abs_url, res) = parse_url("fuchsia-pkg://earth.org/spacex_pkg/variant0#foo").unwrap();
        assert_eq!(&abs_url.to_string(), "fuchsia-pkg://earth.org/spacex_pkg/variant0");
        assert_eq!(res, Some("foo".to_string()));

        let (abs_url, res) = parse_url("fuchsia-pkg://earth.org/spacex_pkg?hash=0000000000000000000000000000000000000000000000000000000000000000#foo").unwrap();
        assert_eq!(&abs_url.to_string(), "fuchsia-pkg://earth.org/spacex_pkg?hash=0000000000000000000000000000000000000000000000000000000000000000");
        assert_eq!(res, Some("foo".to_string()));

        let (abs_url, res) = parse_url("fuchsia-pkg://earth.org/spacex_pkg/variant0?hash=1000000000000000000000000000000000000000000000000000000000000000#foo").unwrap();
        assert_eq!(&abs_url.to_string(), "fuchsia-pkg://earth.org/spacex_pkg/variant0?hash=1000000000000000000000000000000000000000000000000000000000000000");
        assert_eq!(res, Some("foo".to_string()));
    }

    #[fuchsia::test]
    async fn get_pkg_dirs_test() {
        let (resolver, mut stream) =
            create_proxy_and_stream::<fpkg::PackageResolverMarker>().unwrap();
        // Spawn a task to handle the stream of requests
        fasync::Task::spawn(async move {
            while let Some(Ok(request)) = stream.next().await {
                match request {
                    fpkg::PackageResolverRequest::Resolve { responder, package_url: _, dir: _ } => {
                        responder
                            .send(Ok(&fidl_fuchsia_pkg::ResolutionContext { bytes: vec![] }))
                            .unwrap();
                    }
                    _ => {}
                }
            }
        })
        .detach();

        // Empty package list.
        assert!(get_pkg_dirs(vec![], &resolver).await.unwrap().is_empty());

        // Non-empty package list, but with a malformed URL.
        assert_matches!(
            get_pkg_dirs(vec!["".to_string()], &resolver).await,
            Err(LauncherError::BadUrl)
        );

        // Valid package list.
        let v = get_pkg_dirs(vec!["fuchsia-pkg://h/n".to_string()], &resolver).await.unwrap();
        assert!(v.len() == 1);
        assert_eq!(v[0].pkg_url.host().to_string(), "h".to_string());
        assert_eq!(v[0].pkg_url.name().to_string(), "n".to_string());
        assert_eq!(v[0].pkg_url.variant(), None);
        assert_eq!(v[0].pkg_url.hash(), None);
    }

    // Required for assert_matches!().
    impl fmt::Debug for Trampolines {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "map size: {}", self.map.len())
        }
    }

    impl Trampolines {
        fn len(&self) -> usize {
            self.map.len()
        }

        fn get_nth_package(&self, i: usize) -> Option<(&String, &BTreeSet<Trampoline>)> {
            self.map.iter().nth(i)
        }
    }

    async fn open_directory(server: Arc<dyn DirectoryEntry>) -> fio::DirectoryProxy {
        let (proxy, server_end) =
            create_proxy::<fio::DirectoryMarker>().expect("Failed to create connection endpoints");
        let flags = fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE;
        server.open(ExecutionScope::new(), flags, Path::dot(), server_end.into_channel().into());
        proxy
    }

    async fn make_pkg(url: &str, name: &str, root: &Arc<Simple<ImmutableConnection>>) -> PkgDir {
        let (pkg_url, resource) = parse_url(&url.to_string()).unwrap();
        let dir =
            open_directory(root.get_entry(name).expect("error obtaining directory entry")).await;
        PkgDir { pkg_url, dir, resource }
    }

    fn check_trampoline(
        trampolines: &BTreeSet<Trampoline>,
        bin: usize,
        contents: &str,
        binary_name: &str,
    ) {
        let trampoline = trampolines.iter().nth(bin).unwrap();
        assert_eq!(trampoline.contents, contents);
        assert_eq!(trampoline.binary_name, binary_name);
    }

    #[fuchsia::test]
    async fn create_trampolines_test() {
        async fn create_test_directory_proxies() -> Vec<PkgDir> {
            let root = vfs::pseudo_directory! {
                "Nasa" => vfs::pseudo_directory! {
                    "bin" => vfs::pseudo_directory! {
                        "go2moon_v1969" => read_only(b"Apollo"),
                        "go2moon_v2024" => read_only(b"Artemis"),
                    },
                },
                "SpaceX" => vfs::pseudo_directory! {
                    "bin" => vfs::pseudo_directory! {
                        "go2orbit" => read_only(b"Falcon 9"),
                        "go2mars" => read_only(b"Starship"),
                        "bogus_dir" => vfs::pseudo_directory! {"bogus_file" => read_only(b"bogus_content"),}
                    },
                },
                "BlueOrigin" => vfs::pseudo_directory! {
                    "bin" => vfs::pseudo_directory! {},
                },
            };
            vec![
                make_pkg("fuchsia-pkg://earth.org/nasa_pkg", "Nasa", &root).await,
                make_pkg("fuchsia-pkg://earth.org/spacex_pkg", "SpaceX", &root).await,
                make_pkg("fuchsia-pkg://earth.org/blueorigin_pkg", "BlueOrigin", &root).await,
            ]
        }

        let pkg_dirs = create_test_directory_proxies().await;
        let pkg_trampolines = create_trampolines(&pkg_dirs).await.unwrap();
        assert_eq!(pkg_trampolines.len(), 3); // Includes one for "blueorigin_pkg", which had no binaries.
        let (name, list) = pkg_trampolines.get_nth_package(0).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(name, "nasa_pkg");
        check_trampoline(
            &list,
            0,
            "#!resolve fuchsia-pkg://earth.org/nasa_pkg#bin/go2moon_v1969\n",
            "go2moon_v1969",
        );
        check_trampoline(
            &list,
            1,
            "#!resolve fuchsia-pkg://earth.org/nasa_pkg#bin/go2moon_v2024\n",
            "go2moon_v2024",
        );

        let (name, list) = pkg_trampolines.get_nth_package(1).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(name, "spacex_pkg");
        check_trampoline(
            &list,
            0,
            "#!resolve fuchsia-pkg://earth.org/spacex_pkg#bin/go2mars\n",
            "go2mars",
        );
        check_trampoline(
            &list,
            1,
            "#!resolve fuchsia-pkg://earth.org/spacex_pkg#bin/go2orbit\n",
            "go2orbit",
        );

        let (name, list) = pkg_trampolines.get_nth_package(2).unwrap();
        assert_eq!(list.len(), 0);
        assert_eq!(name, "blueorigin_pkg");
    }

    #[fuchsia::test]
    async fn trampolines_binary_collisions_test() {
        // Create two packages containing the same binary name.
        let root = vfs::pseudo_directory! {
            "Nasa" => vfs::pseudo_directory! {
                "bin" => vfs::pseudo_directory! {
                    "collision" => read_only(b"Apollo"),
                },
            },
            "SpaceX" => vfs::pseudo_directory! {
                "bin" => vfs::pseudo_directory! {
                    "collision" => read_only(b"Falcon 9"),
                },
            },
        };
        // But use URLs that have unique URLs.
        let pkg_dirs = vec![
            make_pkg("fuchsia-pkg://earth.org/nasa_pkg", "Nasa", &root).await,
            make_pkg("fuchsia-pkg://earth.org/spacex_pkg", "SpaceX", &root).await,
        ];

        let pkg_trampolines = create_trampolines(&pkg_dirs).await.unwrap();
        // Below, the trampolines are both named `collision`. However, they are in different
        // packages, so do not conflict.
        let (name, list) = pkg_trampolines.get_nth_package(0).unwrap();
        assert_eq!(name, "nasa_pkg");
        assert_eq!(list.len(), 1);
        check_trampoline(
            &list,
            0,
            "#!resolve fuchsia-pkg://earth.org/nasa_pkg#bin/collision\n",
            "collision",
        );
        let (name, list) = pkg_trampolines.get_nth_package(1).unwrap();
        assert_eq!(name, "spacex_pkg");
        assert_eq!(list.len(), 1);
        check_trampoline(
            &list,
            0,
            "#!resolve fuchsia-pkg://earth.org/spacex_pkg#bin/collision\n",
            "collision",
        );

        // This next set includes hashes and variants, which are ignored. So these collide.
        assert_matches!(
            create_trampolines(&vec![
                make_pkg("fuchsia-pkg://earth.org/spacex_pkg", "SpaceX", &root).await,
                make_pkg("fuchsia-pkg://earth.org/spacex_pkg/variant0", "SpaceX", &root).await,
            ])
            .await,
            Err(LauncherError::NonUniqueBinaryName)
        );
        assert_matches!(
            create_trampolines(&vec![
                make_pkg("fuchsia-pkg://earth.org/spacex_pkg", "SpaceX", &root).await,
                make_pkg("fuchsia-pkg://earth.org/spacex_pkg?hash=0000000000000000000000000000000000000000000000000000000000000000", "SpaceX", &root).await,
            ]).await,
            Err(LauncherError::NonUniqueBinaryName)
        );
        assert_matches!(
            create_trampolines(&vec![
                make_pkg("fuchsia-pkg://earth.org/spacex_pkg", "SpaceX", &root).await,
                make_pkg("fuchsia-pkg://earth.org/spacex_pkg/variant0?hash=1000000000000000000000000000000000000000000000000000000000000000", "SpaceX", &root).await,
            ]).await,
            Err(LauncherError::NonUniqueBinaryName)
        );
        assert_matches!(
            create_trampolines(&vec![
                make_pkg("fuchsia-pkg://earth.org/spacex_pkg/variant0", "SpaceX", &root).await,
                make_pkg("fuchsia-pkg://earth.org/spacex_pkg/variant0?hash=1000000000000000000000000000000000000000000000000000000000000000", "SpaceX", &root).await,
            ]).await,
            Err(LauncherError::NonUniqueBinaryName)
        );
    }

    #[fuchsia::test]
    async fn trampolines_package_collisions_test() {
        // Create two packages containing the same package name, but different binaries.
        let root = vfs::pseudo_directory! {
            "UpGoer" => vfs::pseudo_directory! {
                "bin" => vfs::pseudo_directory! {
                    "apollo" => read_only(b"Apollo"),
                },
            }
        };
        let root1 = vfs::pseudo_directory! {
            "UpGoer" => vfs::pseudo_directory! {
                "bin" => vfs::pseudo_directory! {
                    "falcon9" => read_only(b"Falcon 9"),
                },
             }
        };
        // The package names, being the same, are merged. The binaries are different, so do not
        // conflict.
        let pkg_trampolines = create_trampolines(&vec![
            make_pkg("fuchsia-pkg://earth.org/upgoer_pkg", "UpGoer", &root).await,
            make_pkg("fuchsia-pkg://earth.org/upgoer_pkg", "UpGoer", &root1).await,
        ])
        .await
        .unwrap();
        let (name, list) = pkg_trampolines.get_nth_package(0).unwrap();
        assert_eq!(name, "upgoer_pkg");
        assert_eq!(list.len(), 2);
        check_trampoline(
            &list,
            0,
            "#!resolve fuchsia-pkg://earth.org/upgoer_pkg#bin/apollo\n",
            "apollo",
        );
        check_trampoline(
            &list,
            1,
            "#!resolve fuchsia-pkg://earth.org/upgoer_pkg#bin/falcon9\n",
            "falcon9",
        );
    }

    #[fuchsia::test]
    async fn make_trampoline_vfs_test_empty() {
        let (dirs, path) = make_trampoline_vfs(Trampolines::new()).await.unwrap();
        assert_matches!(dirs, None);
        assert_matches!(path, None);
    }

    #[fuchsia::test]
    async fn make_trampoline_vfs_test_if_package_repeated() {
        let mut set = BTreeSet::new();
        set.insert(Trampoline { contents: "foo".to_string(), binary_name: "bar".to_string() });
        let mut set2 = BTreeSet::new();
        set2.insert(Trampoline { contents: "foo2".to_string(), binary_name: "bar2".to_string() });
        let mut trampolines = Trampolines::new();
        trampolines.insert("pkg_foobar".to_string(), set).unwrap();
        trampolines.insert("pkg_foobar".to_string(), set2).unwrap();
        let (dir, path) = make_trampoline_vfs(trampolines).await.unwrap();
        assert!(dir.is_some());
        // There is only one entry in the path for pkg_foobar.
        assert_eq!(path.unwrap(), "/.dash/tools/pkg_foobar");
    }

    #[fuchsia::test]
    async fn make_trampoline_vfs_test() {
        async fn contents_of(path: &str, dir: &fio::DirectoryProxy) -> String {
            let file =
                open_file(dir, path, OpenFlags::RIGHT_READABLE).await.expect("could not open file");
            read_to_string(&file)
                .await
                .unwrap_or_else(|e| panic!("could not open file: {}: {:?}", path, e))
        }

        let mut pkg_trampolines = Trampolines::new();
        pkg_trampolines
            .insert(
                "pkg_foobar".to_string(),
                BTreeSet::from([
                    Trampoline {
                        contents: "#!resolve foo".to_string(),
                        binary_name: "foo".to_string(),
                    },
                    Trampoline {
                        contents: "#!resolve bar".to_string(),
                        binary_name: "bar".to_string(),
                    },
                ]),
            )
            .unwrap();
        pkg_trampolines
            .insert(
                "pkg_foobar2".to_string(),
                BTreeSet::from([
                    Trampoline {
                        contents: "#!resolve foo2".to_string(),
                        binary_name: "foo2".to_string(),
                    },
                    Trampoline {
                        contents: "#!resolve bar2".to_string(),
                        binary_name: "bar2".to_string(),
                    },
                ]),
            )
            .unwrap();
        let (dirs, path) = make_trampoline_vfs(pkg_trampolines).await.unwrap();

        // Check the path. Order is insertion order.
        assert_eq!(path, Some("/.dash/tools/pkg_foobar:/.dash/tools/pkg_foobar2".to_string()));

        // Check the directory.
        let dir = dirs.unwrap();
        assert_eq!(
            fuchsia_fs::directory::readdir(&dir).await.unwrap(),
            vec![
                fuchsia_fs::directory::DirEntry {
                    name: "pkg_foobar".to_string(),
                    kind: fuchsia_fs::directory::DirentKind::Directory
                },
                fuchsia_fs::directory::DirEntry {
                    name: "pkg_foobar2".to_string(),
                    kind: fuchsia_fs::directory::DirentKind::Directory
                },
            ]
        );

        assert_eq!(&contents_of("pkg_foobar/foo", &dir).await, "#!resolve foo");
        assert_eq!(&contents_of("pkg_foobar/bar", &dir).await, "#!resolve bar");
        assert_eq!(&contents_of("pkg_foobar2/foo2", &dir).await, "#!resolve foo2");
        assert_eq!(&contents_of("pkg_foobar2/bar2", &dir).await, "#!resolve bar2");
    }

    #[fuchsia::test]
    async fn create_env_path_test() {
        assert_eq!(create_env_path(Some("bar".to_string())), "PATH=bar");
        assert_eq!(create_env_path(Some("".to_string())), "PATH=");
        assert_eq!(create_env_path(Some(" ".to_string())), "PATH= ");
        assert_eq!(create_env_path(None), "");
    }
}
