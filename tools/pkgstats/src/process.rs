// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::types::{
    Capability, ComponentContents, ElfContents, FileInfo, FileMetadata, OtherContents,
    OutputSummary, PackageContents, PackageFile, ProtocolToClientMap,
};
use anyhow::Result;
use argh::FromArgs;
use assembly_manifest::{AssemblyManifest, Image, PackageSetMetadata, PackagesMetadata};
use camino::{Utf8Path, Utf8PathBuf};
use fidl_fuchsia_component_decl as fdecl;
use fuchsia_archive::Reader as FARReader;
use fuchsia_pkg::PackageManifest;
use fuchsia_url::UnpinnedAbsolutePackageUrl;
use log::debug;
use rayon::prelude::*;
use rayon::ThreadPoolBuilder;
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};
use std::env::current_exe;
use std::fmt::Debug;
use std::fs::File;
use std::io::Read;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(FromArgs)]
#[argh(subcommand, name = "process")]
/// process an out directory into a JSON representation
pub struct ProcessCommand {
    /// the path to the assembly manifest file.
    #[argh(option)]
    assembly_manifest: Utf8PathBuf,

    /// the path to save the output json file
    #[argh(option)]
    out: Utf8PathBuf,

    /// if set, process manifests one at a time, for debugging.
    #[argh(switch)]
    debug_no_parallel: bool,
}

impl ProcessCommand {
    pub fn execute(self) -> Result<()> {
        let manifest = AssemblyManifest::try_load_from(&self.assembly_manifest)?;

        if self.debug_no_parallel {
            ThreadPoolBuilder::new().num_threads(1).build_global().expect("make thread pool");
        }

        let errors = Errors::default();
        let manifest_count = AtomicUsize::new(0);
        let names = Mutex::new(HashMap::new());
        let content_hash_to_path = Mutex::new(HashMap::new());
        let start = Instant::now();

        let assembly_manifest_dir_path = self.assembly_manifest.parent().unwrap();
        manifest
            .images
            .into_par_iter()
            .flat_map_iter(|image_manifest| -> Box<dyn Iterator<Item = Utf8PathBuf>> {
                // TODO(https://fxbug.dev/401590492): we must extend this with Bootfs packages too.
                let packages = match image_manifest {
                    Image::Dtbo(_)
                    | Image::FVM(_)
                    | Image::FVMSparse(_)
                    | Image::FVMFastboot(_)
                    | Image::QemuKernel(_)
                    | Image::VBMeta(_)
                    | Image::ZBI { path: _, signed: _ } => return Box::new(std::iter::empty()),
                    // We skip this one, its contents are listed in the blobfs and fxfs contents.
                    Image::BasePackage(_) => return Box::new(std::iter::empty()),
                    Image::BlobFS { path: _, contents }
                    | Image::Fxfs { path: _, contents }
                    | Image::FxfsSparse { path: _, contents } => contents.packages,
                };
                let PackagesMetadata {
                    base: PackageSetMetadata(base_packages),
                    cache: PackageSetMetadata(cache_packages),
                } = packages;
                Box::new(base_packages.into_iter().chain(cache_packages.into_iter()).map(
                    |metadata| {
                        // This path is relative to the assembly manifest path.
                        absolute_path_for(&assembly_manifest_dir_path, &metadata.manifest)
                    },
                ))
            })
            .for_each(|manifest_path| {
                manifest_count.fetch_add(1, Ordering::Relaxed);
                let manifest = match PackageManifest::try_load_from(&manifest_path) {
                    Ok(m) => m,
                    Err(err) => {
                        errors.log_manifest_error(err, &manifest_path, "loading manifest");
                        return;
                    }
                };
                if let Some((url, contents)) = process_package_manifest(
                    manifest,
                    manifest_path,
                    &errors,
                    &content_hash_to_path,
                ) {
                    names.lock().unwrap().insert(url, contents);
                }
            });
        let file_infos = Mutex::new(HashMap::new());
        let elf_count = AtomicUsize::new(0);
        let other_count = AtomicUsize::new(0);
        let interner = InternEnumerator::new();

        let debugdump_path = current_exe().expect("get current path").with_file_name("debugdump");
        if !debugdump_path.exists() {
            panic!(
                "Expected to find debugdump binary adjacent to pkgstats here: {:?}",
                debugdump_path
            );
        }

        content_hash_to_path.lock().unwrap().par_iter().for_each(|(hash, path)| {
            // Checks for empty paths, Path::new("").parent is always None.
            if path.parent().is_none() {
                debug!("Skipping, no path");
                return;
            }

            let path = Utf8PathBuf::from(path);
            let alt_path = path
                .parent()
                .map(|v| v.join("exe.unstripped").join(path.file_name().unwrap_or_default()));
            let path = if let Some(alt_path) = alt_path {
                if alt_path.is_file() {
                    alt_path
                } else {
                    path
                }
            } else {
                path
            };

            debug!("Found canonical path at {path}");

            if !path.exists() {
                eprintln!("The file '{path}' doesn't exist. Skipping.");
                return;
            }
            let mut f = match File::open(&path) {
                Ok(f) => f,
                Err(err) => {
                    eprintln!("Failed to open {path}, skipping: {err:?}");
                    return;
                }
            };
            let mut header_buf = [0u8; 4];
            // Check if this looks like an ELF file, starting with 0x7F 'E' 'L' 'F'
            if f.read_exact(&mut header_buf).is_ok()
                && header_buf == [0x7fu8, 0x45u8, 0x4cu8, 0x46u8]
            {
                // process
                elf_count.fetch_add(1, Ordering::Relaxed);
                debug!("Looks like ELF, dumping headers");

                let mut elf_contents = ElfContents::new(path.to_string());
                let proc = std::process::Command::new(&debugdump_path)
                    .arg(path.as_os_str())
                    .output()
                    .expect("running debugdump");

                let output = serde_json::from_slice::<DebugDumpOutput>(&proc.stdout);
                let files = match output {
                    Ok(output) => {
                        if output.status != *"OK" {
                            debug!("Dumping failed, {}", output.error);
                            eprintln!("Debug info error: {}", output.error);
                            vec![]
                        } else {
                            debug!("Dumping succeeded, found {} files", output.files.len());
                            output.files
                        }
                    }
                    Err(e) => {
                        eprintln!("Error parsing debugdump output: {:?}", e);
                        vec![]
                    }
                };
                for line in files.iter() {
                    elf_contents.source_file_references.insert(interner.intern(line));
                }
                file_infos.lock().unwrap().insert(hash.clone(), FileInfo::Elf(elf_contents));
            } else {
                debug!("Looks like some other kind of file");
                file_infos
                    .lock()
                    .unwrap()
                    .insert(hash.clone(), FileInfo::Other(OtherContents { source_path: path }));
                other_count.fetch_add(1, Ordering::Relaxed);
            }
        });

        let duration = Instant::now() - start;

        println!(
        "Loaded in {:?}. {} manifests, {} valid, {} manifest errors, {} file errors. {} ELF / {} Other files found. Contents processed: {}",
        duration,
        manifest_count.load(Ordering::Relaxed),
        names.lock().unwrap().len(),
        errors.manifest_errors.load(Ordering::Relaxed),
        errors.manifest_file_errors.load(Ordering::Relaxed),
        elf_count.load(Ordering::Relaxed),
        other_count.load(Ordering::Relaxed),
        content_hash_to_path.lock().unwrap().len(),
    );

        let start = Instant::now();

        let mut packages = names.lock().unwrap().drain().collect::<BTreeMap<_, _>>();
        let contents = file_infos.lock().unwrap().drain().collect::<BTreeMap<_, _>>();
        let files = interner
            .intern_set
            .lock()
            .unwrap()
            .drain()
            .map(|(k, v)| (v, FileMetadata { source_path: k }))
            .collect::<BTreeMap<_, _>>();

        // Populate a Protocol->(Package, component) client mapping.
        let mut protocol_to_client: ProtocolToClientMap = HashMap::new();
        for (url, package) in packages.iter_mut() {
            for (component_name, component) in package.components.iter_mut() {
                for Capability::Protocol(protocol) in component
                    .used_from_parent
                    .iter()
                    .chain(component.used_from_child.iter().map(|(c, _)| c))
                {
                    let protocol_to_packages =
                        protocol_to_client.entry(protocol.clone()).or_default();
                    let package_to_components =
                        protocol_to_packages.entry(url.clone()).or_default();
                    package_to_components.insert(component_name.clone());
                }
            }
        }

        let output = OutputSummary { packages, contents, files, protocol_to_client };

        let mut file = std::fs::File::create(self.out)?;
        serde_json::to_writer(&mut file, &output)?;
        let dur = Instant::now() - start;
        println!("Output JSON in {:?}", dur);

        Ok(())
    }
}

// Given a file at `../../some/path/file.ext` and a root directory: `/the/root/path/` returns the
// absolute path for: `/the/root/path/../../some/path/file.ext
fn absolute_path_for(root_path: &Utf8Path, relative_path: &Utf8Path) -> Utf8PathBuf {
    Utf8PathBuf::try_from(root_path.join(relative_path).canonicalize().expect("path exists"))
        .expect("assembly related path must be utf8")
}

#[derive(Default)]
struct Errors {
    manifest_errors: AtomicUsize,
    manifest_file_errors: AtomicUsize,
}

impl Errors {
    fn log_manifest_error<E>(&self, err: E, manifest_path: &Utf8PathBuf, step: &str)
    where
        E: Debug,
    {
        self.manifest_errors.fetch_add(1, Ordering::Relaxed);
        debug!(status = "Failed", step; "");
        eprintln!("[{}] Failed {}: {:?}", manifest_path, step, err);
    }

    fn log_manifest_file_error<E>(
        &self,
        err: E,
        manifest_path: &Utf8PathBuf,
        step: &str,
        context: impl AsRef<str>,
    ) where
        E: Debug,
    {
        self.manifest_file_errors.fetch_add(1, Ordering::Relaxed);
        debug!(status = "Failed", step; "");
        eprintln!("[{}] Failed {} for {}: {:?}", manifest_path, step, context.as_ref(), err);
    }
}

fn process_package_manifest(
    manifest: PackageManifest,
    manifest_path: Utf8PathBuf,
    errors: &Errors,
    content_hash_to_path: &Mutex<HashMap<String, Utf8PathBuf>>,
) -> Option<(UnpinnedAbsolutePackageUrl, PackageContents)> {
    let url = match manifest.package_url() {
        Err(err) => {
            errors.log_manifest_error(err, &manifest_path, "formatting URL");
            return None;
        }
        Ok(None) => {
            // Package does not have a URL, skip.
            errors.manifest_errors.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        Ok(Some(url)) => url,
    };

    let mut contents = PackageContents::default();

    debug!("Have {} blobs", manifest.blobs().len());

    for blob in manifest.blobs() {
        let blob_source_path = Utf8PathBuf::from(&blob.source_path);
        if blob.path == "meta/" {
            process_far(&blob_source_path, &mut contents, &errors, &manifest_path);
        } else {
            content_hash_to_path.lock().unwrap().insert(blob.merkle.to_string(), blob_source_path);
            contents
                .files
                .push(PackageFile { name: blob.path.to_string(), hash: blob.merkle.to_string() });
        }
    }
    Some((url, contents))
}

fn process_far(
    far_path: &Utf8PathBuf,
    contents: &mut PackageContents,
    errors: &Errors,
    manifest_path: &Utf8PathBuf,
) {
    // Handle meta
    let meta_file = match File::open(far_path) {
        Ok(meta_file) => meta_file,
        Err(err) => {
            errors.log_manifest_file_error(err, &manifest_path, "opening file", &far_path);
            return;
        }
    };
    let mut reader = match FARReader::new(meta_file) {
        Ok(r) => r,
        Err(err) => {
            errors.log_manifest_file_error(err, &manifest_path, "opening as FAR file", &far_path);
            return;
        }
    };
    let mut component_manifest_paths = vec![];
    debug!("Loaded manifest, have {} entries", reader.list().len());
    for entry in reader.list() {
        let path = String::from_utf8_lossy(entry.path());
        if path.ends_with(".cm") {
            debug!("Found a component manifest, {}", path);
            component_manifest_paths.push(entry.path().to_owned());
        }
    }

    for component_manifest_path in component_manifest_paths {
        let data = match reader.read_file(&component_manifest_path) {
            Ok(d) => d,
            Err(err) => {
                errors.log_manifest_file_error(
                    err,
                    manifest_path,
                    "reading component manifest",
                    String::from_utf8_lossy(&component_manifest_path),
                );
                break;
            }
        };
        let manifest: fdecl::Component = match fidl::unpersist(&data) {
            Ok(m) => m,
            Err(err) => {
                errors.log_manifest_file_error(
                    err,
                    manifest_path,
                    "parsing component manifest",
                    String::from_utf8_lossy(&component_manifest_path),
                );
                break;
            }
        };

        let mut component = ComponentContents::default();
        for cap in manifest.uses.into_iter().flatten() {
            match cap {
                fdecl::Use::Protocol(p) => {
                    let (name, from) = match (p.source_name, p.source) {
                        (Some(s), Some(r)) => (s, r),
                        _ => continue,
                    };
                    match from {
                        fdecl::Ref::Parent(_) => {
                            component.used_from_parent.insert(Capability::Protocol(name));
                        }
                        fdecl::Ref::Child(c) => {
                            component.used_from_child.insert((Capability::Protocol(name), c.name));
                        }
                        // TODO(https://fxbug.dev/347290357): Handle different types of refs
                        e => {
                            debug!("Unknown use from ref: {:?}", e);
                        }
                    }
                }
                // TODO(https://fxbug.dev/347290357): Handle different types of entries
                e => {
                    debug!("Unknown use entry: {:?}", e)
                    // Skip all else for now
                }
            }
        }
        for cap in manifest.exposes.into_iter().flatten() {
            match cap {
                fdecl::Expose::Protocol(p) => {
                    let (name, from) = match (p.source_name, p.source) {
                        (Some(s), Some(r)) => (s, r),
                        _ => continue,
                    };
                    match from {
                        fdecl::Ref::Self_(_) => {
                            component.exposed_from_self.insert(Capability::Protocol(name));
                        }
                        fdecl::Ref::Child(c) => {
                            component
                                .exposed_from_child
                                .insert((Capability::Protocol(name), c.name));
                        }
                        e => {
                            // TODO(https://fxbug.dev/347290357): Handle different types of refs
                            debug!("Unknown expose from ref: {:?}", e);
                        }
                    }
                }
                // TODO(https://fxbug.dev/347290357): Handle different types of entries
                e => {
                    debug!("Unknown exposes entry: {:?}", e)
                    // Skip all else for now
                }
            }
        }
        for cap in manifest.offers.into_iter().flatten() {
            if let fdecl::Offer::Protocol(p) = cap {
                if let (Some(name), Some(from)) = (p.source_name, p.source) {
                    match from {
                        fdecl::Ref::Self_(_) => {
                            component.offered_from_self.insert(Capability::Protocol(name));
                        }
                        fdecl::Ref::Child(_) => {
                            // Do not handle yet
                        }
                        e => {
                            debug!("Unknown offer from ref: {:?}", e);
                        }
                    }
                }
            }
        }

        let path = String::from_utf8_lossy(&component_manifest_path);
        let last_segment = path.rfind("/");
        let name = match last_segment {
            Some(i) => &path[i + 1..],
            None => &path,
        };
        contents.components.insert(name.to_string(), component);
    }
}

#[derive(Clone)]
struct InternEnumerator {
    intern_set: Arc<Mutex<HashMap<String, u32>>>,
    next_id: Arc<AtomicU32>,
}

impl InternEnumerator {
    pub fn new() -> Self {
        Self {
            intern_set: Arc::new(Mutex::new(HashMap::new())),
            next_id: Arc::new(AtomicU32::new(0)),
        }
    }
    pub fn intern(&self, value: &str) -> u32 {
        let mut set = self.intern_set.lock().unwrap();
        if let Some(val) = set.get(value) {
            *val
        } else {
            let next = self.next_id.fetch_add(1, Ordering::Relaxed);
            set.insert(value.to_string(), next);
            next
        }
    }
}

#[derive(Deserialize)]
struct DebugDumpOutput {
    status: String,
    error: String,
    files: Vec<String>,
}
