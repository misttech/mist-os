// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        core::{
            collection::{
                Capability, Component, ComponentSource, Components, CoreDataDeps, Manifest,
                ManifestData, Manifests, Package, Packages, ProtocolCapability, Zbi,
            },
            package::{
                is_cf_v2_manifest,
                reader::{
                    read_partial_package_definition, PackageReader, PackagesFromUpdateReader,
                },
            },
            util::types::{
                ComponentManifest, PackageDefinition, PartialPackageDefinition, ServiceMapping,
            },
        },
        static_pkgs::StaticPkgsCollection,
    },
    anyhow::{anyhow, bail, Context, Result},
    cm_fidl_analyzer::{match_absolute_pkg_urls, PkgUrlMatch},
    cm_fidl_validator,
    fidl::unpersist,
    fidl_fuchsia_component_decl as fdecl,
    fuchsia_merkle::Hash,
    fuchsia_url::{
        boot_url::BootUrl, AbsoluteComponentUrl, AbsolutePackageUrl, PackageName, PackageVariant,
    },
    once_cell::sync::Lazy,
    scrutiny::model::{collector::DataCollector, model::DataModel},
    scrutiny_config::ModelConfig,
    scrutiny_utils::{
        artifact::{ArtifactReader, FileArtifactReader},
        bootfs::BootfsReader,
        key_value::parse_key_value,
        zbi::{ZbiReader, ZbiType},
    },
    std::{
        collections::{HashMap, HashSet},
        io::Cursor,
        path::{Path, PathBuf},
        str,
        sync::Arc,
    },
    tracing::{info, warn},
    update_package::parse_image_packages_json,
    url::Url,
};

// Constants/Statics
static FUCHSIA_ZBI_PATH: Lazy<PathBuf> = Lazy::new(|| PathBuf::from("zbi"));
static FUCHSIA_ZBI_SIGNED_PATH: Lazy<PathBuf> = Lazy::new(|| PathBuf::from("zbi.signed"));
static RECOVERY_ZBI_PATH: Lazy<PathBuf> = Lazy::new(|| PathBuf::from("recovery"));
static RECOVERY_ZBI_SIGNED_PATH: Lazy<PathBuf> = Lazy::new(|| PathBuf::from("recovery.signed"));
static IMAGES_JSON_PATH: Lazy<PathBuf> = Lazy::new(|| PathBuf::from("images.json"));
static IMAGES_JSON_ORIG_PATH: Lazy<PathBuf> = Lazy::new(|| PathBuf::from("images.json.orig"));

// The path to the package index file for bootfs packages in gendir and
// in the bootfs.
const BOOTFS_PACKAGE_INDEX: &str = "data/bootfs_packages";

// The root v2 component manifest.
pub const ROOT_RESOURCE: &str = "meta/root.cm";

struct StaticPackageDescription<'a> {
    pub name: &'a PackageName,
    pub variant: Option<&'a PackageVariant>,
    pub merkle: &'a Hash,
}

impl<'a> StaticPackageDescription<'a> {
    fn new(name: &'a PackageName, variant: Option<&'a PackageVariant>, merkle: &'a Hash) -> Self {
        Self { name, variant, merkle }
    }

    /// Definitions match when:
    /// 1. Package definition has no hash (warning emitted above), or hashes match;
    /// 2. Names match;
    /// 3. A variant is missing (warning emitted above), or variants match.
    fn matches(&self, pkg: &PackageDefinition) -> bool {
        let url = AbsolutePackageUrl::new(
            pkg.url.repository().clone(),
            self.name.clone(),
            self.variant.map(PackageVariant::clone),
            Some(self.merkle.clone()),
        );
        let url_match = match_absolute_pkg_urls(&url, &pkg.url);
        if url_match == PkgUrlMatch::WeakMatch {
            warn!(
                StaticPackageDescription.url = %url,
                PackageDefinition.url = %pkg.url,
                "Lossy match of absolute package URLs",
            );
        }
        url_match != PkgUrlMatch::NoMatch
    }
}

/// The PackageDataResponse contains all of the core model information extracted
/// from the Fuchsia Archive (.far) packages from the current build.
pub struct PackageDataResponse {
    pub components: HashMap<Url, Component>,
    pub packages: Vec<Package>,
    pub manifests: Vec<Manifest>,
    pub zbi: Option<Zbi>,
}

impl PackageDataResponse {
    pub fn new(
        components: HashMap<Url, Component>,
        packages: Vec<Package>,
        manifests: Vec<Manifest>,
        zbi: Option<Zbi>,
    ) -> Self {
        Self { components, packages, manifests, zbi }
    }
}

/// The PackageDataCollector is a core collector in Scrutiny that is
/// responsible for extracting data from Fuchsia Archives (.far). This collector
/// scans every single package and extracts all of the manifests and files.
/// Using this raw data it constructs all the routes and components in the
/// model.
#[derive(Default)]
pub struct PackageDataCollector {}

impl PackageDataCollector {
    /// Retrieves the set of packages from the current target build returning
    /// them sorted by package url.
    fn get_packages(package_reader: &mut Box<dyn PackageReader>) -> Result<Vec<PackageDefinition>> {
        let package_urls =
            package_reader.read_package_urls().context("Failed to read listing of package URLs")?;
        let mut pkgs = package_urls
            .into_iter()
            .map(|pkg_url| package_reader.read_package_definition(&pkg_url))
            .collect::<Result<Vec<PackageDefinition>>>()
            .context("Failed to read package definition")?;
        pkgs.sort_by(|lhs, rhs| lhs.url.name().cmp(&rhs.url.name()));
        Ok(pkgs)
    }

    /// Extracts the ZBI from the update package and parses it into the ZBI
    /// model.
    fn extract_zbi_from_update_package(
        reader: &mut Box<dyn ArtifactReader>,
        update_package: &PartialPackageDefinition,
        fuchsia_packages: &Vec<PackageDefinition>,
        recovery: bool,
    ) -> Result<Zbi> {
        info!("Extracting the ZBI from update package");

        let result_from_update_package =
            Self::lookup_zbi_hash_in_update_package(update_package, recovery);
        let result_from_images_json = Self::lookup_zbi_hash_in_images_json(
            reader,
            update_package,
            fuchsia_packages,
            recovery,
        );
        let zbi_hash = match (result_from_update_package, result_from_images_json) {
            (Ok(zbi_hash_from_update_package), Ok(zbi_hash_from_images_json)) => {
                if zbi_hash_from_update_package != zbi_hash_from_images_json {
                    return Err(anyhow!(
                        "Update package and its images manifest contain different fuchsia ZBI images: {} != {}",
                        zbi_hash_from_update_package,
                        zbi_hash_from_images_json
                    ));
                }
                zbi_hash_from_images_json
            }
            (_, Ok(zbi_hash)) => zbi_hash,
            (Ok(zbi_hash), _) => zbi_hash,
            (_, Err(err_from_images_json)) => return Err(err_from_images_json),
        };

        let zbi_data = reader.read_bytes(&Path::new(&zbi_hash.to_string()))?;
        let mut reader = ZbiReader::new(zbi_data);
        let sections = reader.parse()?;
        let mut bootfs = HashMap::new();
        let mut cmdline = String::new();
        info!(total = sections.len(), "Extracted sections from the ZBI");
        for section in sections.iter() {
            info!(section_type = ?section.section_type, "Extracted sections");
            if section.section_type == ZbiType::StorageBootfs {
                let mut bootfs_reader = BootfsReader::new(section.buffer.clone());
                let bootfs_result = bootfs_reader.parse();
                if let Err(err) = bootfs_result {
                    warn!(%err, "Bootfs parse failed");
                } else {
                    bootfs = bootfs_result.unwrap();
                    info!(total = bootfs.len(), "Bootfs found files");
                }
            } else if section.section_type == ZbiType::Cmdline {
                let mut cmd_str = std::str::from_utf8(&section.buffer)?;
                if let Some(stripped) = cmd_str.strip_suffix("\u{0000}") {
                    cmd_str = stripped;
                }
                cmdline.push_str(&cmd_str);
            }
        }
        Ok(Zbi { sections, bootfs, cmdline })
    }

    fn lookup_zbi_hash_in_update_package(
        update_package: &PartialPackageDefinition,
        recovery: bool,
    ) -> Result<Hash> {
        let (signed_zbi_path, zbi_path) = if recovery {
            (&*RECOVERY_ZBI_SIGNED_PATH, &*RECOVERY_ZBI_PATH)
        } else {
            (&*FUCHSIA_ZBI_SIGNED_PATH, &*FUCHSIA_ZBI_PATH)
        };
        update_package
            .contents
            .get(signed_zbi_path)
            .or(update_package.contents.get(zbi_path))
            .map(Hash::clone)
            .ok_or(anyhow!(
                "Update package contains no {} zbi image",
                if recovery { "recovery" } else { "fuchsia" }
            ))
    }

    fn lookup_zbi_hash_in_images_json(
        reader: &mut Box<dyn ArtifactReader>,
        update_package: &PartialPackageDefinition,
        fuchsia_packages: &Vec<PackageDefinition>,
        recovery: bool,
    ) -> Result<Hash> {
        let images_json_hash = update_package
            .contents
            .get(&*IMAGES_JSON_PATH)
            .or(update_package.contents.get(&*IMAGES_JSON_ORIG_PATH))
            .ok_or(anyhow!("Update package contains no images manifest entry"))?;
        let images_json_contents = reader
            .read_bytes(&Path::new(&images_json_hash.to_string()))
            .context("Failed to open images manifest blob designated in update package")?;
        let image_packages_manifest = parse_image_packages_json(images_json_contents.as_slice())
            .context("Failed to parse images manifest in update package")?;
        let metadata = if recovery {
            image_packages_manifest.recovery().ok_or(anyhow!(
                "Update package images manifest contains no recovery boot slot images"
            ))
        } else {
            image_packages_manifest.fuchsia().ok_or(anyhow!(
                "Update package images manifest contains no fuchsia boot slot images"
            ))
        }?;
        let zbi_component_url = metadata.zbi().url();
        let zbi_path = PathBuf::from(zbi_component_url.resource());
        let zbi_package_url = zbi_component_url.package_url();

        let images_package = fuchsia_packages
            .iter()
            .find(|&pkg_def| &pkg_def.url == zbi_package_url)
            .ok_or_else(|| {
                anyhow!(
                    "Failed to locate update package images package with URL {}",
                    zbi_package_url
                )
            })?;
        images_package.contents.get(&zbi_path).map(Hash::clone).ok_or(anyhow!(
            "Update package images package contains no {} zbi entry {:?}",
            if recovery { "recovery" } else { "fuchsia" },
            zbi_path
        ))
    }

    fn get_non_bootfs_pkg_source<'a>(
        pkg: &'a PackageDefinition,
        static_pkgs: &'a Option<Vec<StaticPackageDescription<'a>>>,
    ) -> Result<ComponentSource> {
        let pkg_merkle = pkg.url.hash().ok_or_else(||
            anyhow!("Unable to report precise component source from package URL that is missing package hash: {}", pkg.url)
        )?;

        if static_pkgs.is_none() {
            return Ok(ComponentSource::Package(pkg_merkle.clone()));
        }

        for static_pkg in static_pkgs.as_ref().unwrap().iter() {
            if static_pkg.matches(pkg) {
                return Ok(ComponentSource::StaticPackage(static_pkg.merkle.clone()));
            }
        }

        Ok(ComponentSource::Package(pkg_merkle.clone()))
    }

    fn get_static_pkgs<'a>(
        static_pkgs_result: &'a Result<Arc<StaticPkgsCollection>>,
    ) -> Option<Vec<StaticPackageDescription<'a>>> {
        static_pkgs_result
            .as_ref()
            .ok()
            .map(|result| {
                // Collection is only meaningful if there are static packages and no errors.
                if result.static_pkgs.is_some() && result.errors.len() == 0 {
                    Some(
                        result
                            .static_pkgs
                            .as_ref()
                            .unwrap()
                            .iter()
                            .map(|((name, variant), merkle)| {
                                StaticPackageDescription::new(name, variant.as_ref(), merkle)
                            })
                            .collect(),
                    )
                } else {
                    None
                }
            })
            .unwrap_or(None)
    }

    fn get_static_pkg_deps(
        static_pkgs_result: &Result<Arc<StaticPkgsCollection>>,
    ) -> HashSet<PathBuf> {
        static_pkgs_result.as_ref().ok().map(|result| result.deps.clone()).unwrap_or(HashSet::new())
    }

    /// Extracts all of the components and manifests from a package.
    fn extract_package_data<'a>(
        component_id: &mut i32,
        service_map: &mut ServiceMapping,
        components: &mut HashMap<Url, Component>,
        manifests: &mut Vec<Manifest>,
        pkg: &PackageDefinition,
        source: &ComponentSource,
    ) -> Result<()> {
        // Extract components from the packages.
        for (path, cm) in &pkg.cms {
            if !is_cf_v2_manifest(path) {
                continue;
            }

            let path_str = path.to_str().ok_or_else(|| {
                anyhow!("Cannot format component manifest path as string: {:?}", path)
            })?;
            *component_id += 1;
            let url = AbsoluteComponentUrl::from_package_url_and_resource(
                pkg.url.clone(),
                path_str.to_string(),
            )
            .with_context(|| {
                format!(
                    r#"Failed to apply resource path "{}" to package URL "{}""#,
                    path_str, pkg.url
                )
            })?;
            let url = Url::parse(&url.to_string()).with_context(|| {
                format!("Failed to convert package URL to standard URL: {}", url)
            })?;

            components.insert(
                url.clone(),
                Component { id: *component_id, url: url.clone(), source: source.clone() },
            );

            let cf_manifest = {
                if let ComponentManifest::Version2(decl_bytes) = &cm {
                    let mut cap_uses = Vec::new();
                    let cm_base64 = base64::encode(&decl_bytes);
                    let mut cvf_bytes = None;

                    if let Ok(cm_decl) = unpersist::<fdecl::Component>(&decl_bytes) {
                        if let Err(err) = cm_fidl_validator::validate(&cm_decl) {
                            warn!(%err, %url, "Invalid cm");
                        } else {
                            if let Some(schema) = cm_decl.config {
                                match schema
                                    .value_source
                                    .as_ref()
                                    .context("getting value source from config schema")?
                                {
                                    fdecl::ConfigValueSource::PackagePath(pkg_path) => {
                                        cvf_bytes = Some(
                                            pkg.cvfs
                                                .get(pkg_path)
                                                .context("getting config values from package")?
                                                .clone(),
                                        );
                                    }
                                    other => {
                                        warn!("unsupported config value source {:?}", other)
                                    }
                                }
                            }

                            if let Some(uses) = cm_decl.uses {
                                for use_ in uses {
                                    match &use_ {
                                        fdecl::Use::Protocol(protocol) => {
                                            if let Some(source_name) = &protocol.source_name {
                                                cap_uses.push(Capability::Protocol(
                                                    ProtocolCapability::new(source_name.clone()),
                                                ));
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
                            if let Some(exposes) = cm_decl.exposes {
                                for expose in exposes {
                                    match &expose {
                                        fdecl::Expose::Protocol(protocol) => {
                                            if let Some(source_name) = &protocol.source_name {
                                                if let Some(fdecl::Ref::Self_(_)) = &protocol.source
                                                {
                                                    service_map
                                                        .insert(source_name.clone(), url.clone());
                                                }
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                    } else {
                        warn!(%url, "cm failed to be decoded");
                    }
                    Manifest {
                        component_id: *component_id,
                        manifest: ManifestData { cm_base64, cvf_bytes },
                        uses: cap_uses,
                    }
                } else {
                    Manifest {
                        component_id: *component_id,
                        manifest: ManifestData { cm_base64: String::from(""), cvf_bytes: None },
                        uses: Vec::new(),
                    }
                }
            };
            manifests.push(cf_manifest);
        }
        Ok(())
    }

    /// Extracts all the components and manifests from the ZBI.
    fn extract_zbi_data(
        component_id: &mut i32,
        service_map: &mut ServiceMapping,
        components: &mut HashMap<Url, Component>,
        manifests: &mut Vec<Manifest>,
        zbi: &Zbi,
    ) -> Result<()> {
        let mut package_index_found = false;

        for (file_name, file_data) in &zbi.bootfs {
            if file_name == BOOTFS_PACKAGE_INDEX {
                // Ensure that we only find a single package index file in bootfs.
                if package_index_found {
                    bail!("Multiple bootfs package index files found");
                }
                package_index_found = true;

                let bootfs_pkg_index_contents = std::str::from_utf8(&file_data)?;
                let bootfs_pkg_index = parse_key_value(bootfs_pkg_index_contents)?;
                Self::extract_bootfs_packaged_data(
                    component_id,
                    service_map,
                    components,
                    manifests,
                    zbi,
                    bootfs_pkg_index,
                )?;
            } else if file_name.ends_with(".cm") {
                Self::extract_bootfs_unpackaged_data(
                    component_id,
                    service_map,
                    components,
                    manifests,
                    zbi,
                    file_name,
                    file_data,
                )?;
            }
        }

        Ok(())
    }

    fn extract_bootfs_unpackaged_data(
        component_id: &mut i32,
        service_map: &mut ServiceMapping,
        components: &mut HashMap<Url, Component>,
        manifests: &mut Vec<Manifest>,
        zbi: &Zbi,
        file_name: &str,
        file_data: &Vec<u8>,
    ) -> Result<()> {
        info!(%file_name, "Extracting bootfs manifest");
        let url = BootUrl::new_resource("/".to_string(), file_name.to_string())?;
        let url = Url::parse(&url.to_string())
            .with_context(|| format!("Failed to convert boot URL to standard URL: {}", url))?;

        Self::extract_bootfs_data(
            component_id,
            service_map,
            components,
            manifests,
            &zbi.bootfs,
            &url,
            file_name,
            file_data,
        )
    }

    fn extract_bootfs_packaged_data(
        component_id: &mut i32,
        service_map: &mut ServiceMapping,
        components: &mut HashMap<Url, Component>,
        manifests: &mut Vec<Manifest>,
        zbi: &Zbi,
        package_index: HashMap<String, String>,
    ) -> Result<()> {
        for (name_and_variant, merkle) in package_index {
            let package_path = format!("blob/{}", merkle);
            let far_cursor = Cursor::new(zbi.bootfs.get(&package_path).ok_or_else(|| {
                anyhow!("Zbi does not contain meta.far at path: {:?}", package_path)
            })?);
            let partial_package_def = read_partial_package_definition(far_cursor)?;

            for (path, cm) in &partial_package_def.cms {
                let path_str = path.to_str().ok_or_else(|| {
                    anyhow!("Cannot format component manifest path as string: {:?}", path)
                })?;
                match cm {
                    ComponentManifest::Version2(bytes) => {
                        let url = BootUrl::new_resource_without_variant(
                            format!("/{}", name_and_variant),
                            path_str.to_string(),
                        )?;
                        let url = Url::parse(&url.to_string()).with_context(|| {
                            format!("Failed to convert boot URL to standard URL: {}", url)
                        })?;
                        Self::extract_bootfs_data(
                            component_id,
                            service_map,
                            components,
                            manifests,
                            &partial_package_def.cvfs,
                            &url,
                            path_str,
                            bytes,
                        )?;
                    }
                    _ => anyhow::bail!("Bootfs only supports V2 components."),
                }
            }
        }

        Ok(())
    }

    fn extract_bootfs_data(
        component_id: &mut i32,
        service_map: &mut ServiceMapping,
        components: &mut HashMap<Url, Component>,
        manifests: &mut Vec<Manifest>,
        cvf_source: &HashMap<String, Vec<u8>>,
        url: &Url,
        file_name: &str,
        file_data: &Vec<u8>,
    ) -> Result<()> {
        info!(%file_name, "Extracting bootfs manifest");
        let cm_base64 = base64::encode(&file_data);
        if let Ok(cm_decl) = unpersist::<fdecl::Component>(&file_data) {
            if let Err(err) = cm_fidl_validator::validate(&cm_decl) {
                warn!(%file_name, %err, "Invalid cm");
            } else {
                // Retrieve this component's config values, if any.
                let cvf_bytes = if let Some(schema) = cm_decl.config {
                    match schema
                        .value_source
                        .as_ref()
                        .context("getting value source from config schema")?
                    {
                        fdecl::ConfigValueSource::PackagePath(pkg_path) => {
                            cvf_source.get(pkg_path).cloned()
                        }
                        other => {
                            anyhow::bail!("Unsupported config value source {:?}.", other);
                        }
                    }
                } else {
                    None
                };

                let mut cap_uses = Vec::new();
                if let Some(uses) = cm_decl.uses {
                    for use_ in uses {
                        match &use_ {
                            fdecl::Use::Protocol(protocol) => {
                                if let Some(source_name) = &protocol.source_name {
                                    cap_uses.push(Capability::Protocol(ProtocolCapability::new(
                                        source_name.clone(),
                                    )));
                                }
                            }
                            _ => {}
                        }
                    }
                }
                if let Some(exposes) = cm_decl.exposes {
                    for expose in exposes {
                        match &expose {
                            fdecl::Expose::Protocol(protocol) => {
                                if let Some(source_name) = &protocol.source_name {
                                    if let Some(fdecl::Ref::Self_(_)) = &protocol.source {
                                        service_map.insert(source_name.clone(), url.clone());
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }

                // The root manifest is special semantically as it offers from its parent
                // which is outside of the component model. So in this case offers
                // should also be captured.
                if file_name == ROOT_RESOURCE {
                    if let Some(offers) = cm_decl.offers {
                        for offer in offers {
                            match &offer {
                                fdecl::Offer::Protocol(protocol) => {
                                    if let Some(source_name) = &protocol.source_name {
                                        if let Some(fdecl::Ref::Parent(_)) = &protocol.source {
                                            service_map.insert(source_name.clone(), url.clone());
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }

                // Add the components directly from the ZBI.
                *component_id += 1;
                components.insert(
                    url.clone(),
                    Component {
                        id: *component_id,
                        url: url.clone(),
                        source: ComponentSource::ZbiBootfs,
                    },
                );
                manifests.push(Manifest {
                    component_id: *component_id,
                    manifest: ManifestData { cm_base64, cvf_bytes },
                    uses: cap_uses,
                });
            }
        }

        Ok(())
    }

    /// Function to build the component graph model out of the packages and services retrieved
    /// by this collector.
    fn extract<'a>(
        update_package: &PartialPackageDefinition,
        mut artifact_reader: &mut Box<dyn ArtifactReader>,
        fuchsia_packages: Vec<PackageDefinition>,
        mut service_map: ServiceMapping,
        static_pkgs: &'a Option<Vec<StaticPackageDescription<'a>>>,
        recovery: bool,
    ) -> Result<PackageDataResponse> {
        let mut components: HashMap<Url, Component> = HashMap::new();
        let mut packages: Vec<Package> = Vec::new();
        let mut manifests: Vec<Manifest> = Vec::new();

        // Iterate through all served packages, for each manifest they define, create a node.
        let mut component_id = 0;
        info!(total = fuchsia_packages.len(), "Found package");
        for pkg in fuchsia_packages.iter() {
            info!(url = %pkg.url, "Extracting package");
            let merkle = pkg.url.hash().ok_or_else(||
                anyhow!("Unable to extract precise package information from URL without package hash: {}", pkg.url)
            )?.clone();
            let package = Package {
                name: pkg.url.name().clone(),
                variant: pkg.url.variant().map(|variant| variant.clone()),
                merkle,
                contents: pkg.contents.clone(),
                meta: pkg.meta.clone(),
            };
            packages.push(package);

            let source = Self::get_non_bootfs_pkg_source(pkg, static_pkgs)?;
            Self::extract_package_data(
                &mut component_id,
                &mut service_map,
                &mut components,
                &mut manifests,
                &pkg,
                &source,
            )?;
        }

        let zbi = match PackageDataCollector::extract_zbi_from_update_package(
            &mut artifact_reader,
            update_package,
            &fuchsia_packages,
            recovery,
        ) {
            Ok(zbi) => {
                Self::extract_zbi_data(
                    &mut component_id,
                    &mut service_map,
                    &mut components,
                    &mut manifests,
                    &zbi,
                )?;
                Some(zbi)
            }
            Err(err) => {
                warn!(%err);
                None
            }
        };

        info!(components = components.len(), manifests = manifests.len());

        Ok(PackageDataResponse::new(components, packages, manifests, zbi))
    }

    pub fn collect_with_reader(
        config: ModelConfig,
        mut package_reader: Box<dyn PackageReader>,
        mut artifact_reader: Box<dyn ArtifactReader>,
        model: Arc<DataModel>,
    ) -> Result<()> {
        let served_packages =
            Self::get_packages(&mut package_reader).context("Failed to read packages listing")?;
        info!(packages = served_packages.len(), "Done collecting. Found listed in the sys realm");

        let update_package = package_reader
            .read_update_package_definition()
            .context("Failed to read update package definition for package data collector")?;
        let static_pkgs_result = model.get();
        let static_pkgs = Self::get_static_pkgs(&static_pkgs_result);
        let response = PackageDataCollector::extract(
            &update_package,
            &mut artifact_reader,
            served_packages,
            ServiceMapping::new(),
            &static_pkgs,
            config.is_recovery(),
        )?;

        let mut model_comps = vec![];
        for (_, val) in response.components.into_iter() {
            model_comps.push(val);
        }

        // TODO: Collections need to forward deps to controllers.
        model.set(Components::new(model_comps)).context("Failed to store components in model")?;
        model.set(Packages::new(response.packages)).context("Failed to store packages in model")?;
        model
            .set(Manifests::new(response.manifests))
            .context("Failed to store manifests in model")?;

        if let Some(zbi) = response.zbi {
            model.set(zbi)?;
        } else {
            model.remove::<Zbi>();
        }

        let mut deps = Self::get_static_pkg_deps(&static_pkgs_result);
        for dep in package_reader.get_deps().into_iter() {
            deps.insert(dep);
        }
        for dep in artifact_reader.get_deps().into_iter() {
            deps.insert(dep);
        }
        model.set(CoreDataDeps::new(deps)).context("Failed to store core data deps")?;

        Ok(())
    }
}

impl DataCollector for PackageDataCollector {
    /// Collects and builds a DAG of component nodes (with manifests) and routes that
    /// connect the nodes.
    fn collect(&self, model: Arc<DataModel>) -> Result<()> {
        let model_config = model.config();
        let blobs_directory = &model_config.blobs_directory();
        let artifact_reader_for_artifact_reader =
            FileArtifactReader::new(&PathBuf::new(), blobs_directory);
        let artifact_reader_for_package_reader = artifact_reader_for_artifact_reader.clone();

        let package_reader: Box<dyn PackageReader> = Box::new(PackagesFromUpdateReader::new(
            &model_config.update_package_path(),
            Box::new(artifact_reader_for_package_reader),
        ));

        Self::collect_with_reader(
            model.config().clone(),
            package_reader,
            Box::new(artifact_reader_for_artifact_reader),
            model,
        )?;

        Ok(())
    }
}

#[cfg(test)]
pub mod tests {
    use fidl::persist;

    use {
        super::{PackageDataCollector, StaticPackageDescription},
        crate::core::{
            collection::{
                testing::fake_component_src_pkg, Capability, Components, CoreDataDeps,
                ManifestData, Manifests, Packages, ProtocolCapability,
            },
            package::{
                collector::{Component, ComponentSource},
                reader::PackageReader,
                test_utils::{
                    create_model, create_test_cm_map, create_test_package_with_cms,
                    create_test_package_with_contents, create_test_partial_package_with_contents,
                    MockPackageReader,
                },
            },
            util::types::{PackageDefinition, PartialPackageDefinition},
        },
        cm_rust::{ComponentDecl, NativeIntoFidl},
        fidl_fuchsia_component_decl as fdecl,
        fuchsia_hash::{Hash, HASH_SIZE},
        fuchsia_merkle::MerkleTree,
        fuchsia_url::{AbsoluteComponentUrl, AbsolutePackageUrl, PackageName, PackageVariant},
        fuchsia_zbi_abi::zbi_container_header,
        maplit::{hashmap, hashset},
        scrutiny_testing::{artifact::MockArtifactReader, fake::fake_model_config},
        scrutiny_utils::artifact::ArtifactReader,
        sha2::{Digest, Sha256},
        std::{collections::HashMap, convert::TryInto, path::PathBuf, str::FromStr, sync::Arc},
        update_package::{ImageMetadata, ImagePackagesManifest},
        url::Url,
        zerocopy::AsBytes,
    };

    fn zero_content_zbi() -> Vec<u8> {
        zbi_container_header(0).as_bytes().into()
    }

    fn non_zero_content_zbi() -> Vec<u8> {
        zbi_container_header(1).as_bytes().into()
    }

    fn make_v2_manifest_data(decl: ComponentDecl) -> ManifestData {
        let decl_fidl: fdecl::Component = decl.native_into_fidl();
        let cm_base64 = base64::encode(&persist(&decl_fidl).unwrap());
        ManifestData { cm_base64, cvf_bytes: None }
    }

    // Return the sha256 content hash of bytes, not to be confused with the fuchsia merkle root.
    fn content_hash(bytes: &[u8]) -> Hash {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        Hash::from(*AsRef::<[u8; 32]>::as_ref(&hasher.finalize()))
    }

    fn default_pkg() -> PackageDefinition {
        PackageDefinition {
            url: "fuchsia-pkg://fuchsia.com/default/0?hash=0000000000000000000000000000000000000000000000000000000000000000".parse().unwrap(),
            meta: HashMap::new(),
            contents: HashMap::new(),
            cms: HashMap::new(),
            cvfs: HashMap::new(),
        }
    }

    fn empty_update_pkg() -> PartialPackageDefinition {
        PartialPackageDefinition {
            meta: HashMap::new(),
            contents: HashMap::new(),
            cms: HashMap::new(),
            cvfs: HashMap::new(),
        }
    }

    #[fuchsia::test]
    fn test_static_pkgs_matches() {
        let pkg_def_with_variant = PackageDefinition {
            url: "fuchsia-pkg://fuchsia.com/alpha-beta_gamma9/0?hash=0000000000000000000000000000000000000000000000000000000000000000".parse().unwrap(),
            ..default_pkg()
        };
        let pkg_def_without_variant = PackageDefinition {
            url: "fuchsia-pkg://fuchsia.com/alpha-beta_gamma9?hash=0000000000000000000000000000000000000000000000000000000000000000".parse().unwrap(),
            ..default_pkg()
        };
        // Match.
        assert!(StaticPackageDescription::new(
            &PackageName::from_str("alpha-beta_gamma9").unwrap(),
            Some(&PackageVariant::zero()),
            &Hash::from([0u8; HASH_SIZE])
        )
        .matches(&pkg_def_with_variant));
        // Match with self variant None, input variant Some.
        assert!(StaticPackageDescription::new(
            &PackageName::from_str("alpha-beta_gamma9").unwrap(),
            None,
            &Hash::from([0u8; HASH_SIZE])
        )
        .matches(&pkg_def_with_variant));
        // Match with self variant Some, input variant None.
        assert!(StaticPackageDescription::new(
            &PackageName::from_str("alpha-beta_gamma9").unwrap(),
            Some(&PackageVariant::zero()),
            &Hash::from([0u8; HASH_SIZE])
        )
        .matches(&pkg_def_without_variant));
        // Variant mismatch.
        assert!(
            StaticPackageDescription::new(
                &PackageName::from_str("alpha-beta_gamma9").unwrap(),
                Some(&PackageVariant::from_str("1").unwrap()),
                &Hash::from([0u8; HASH_SIZE])
            )
            .matches(&pkg_def_with_variant)
                == false
        );
        // Merkle mismatch.
        assert!(
            StaticPackageDescription::new(
                &PackageName::from_str("alpha").unwrap(),
                Some(&PackageVariant::zero()),
                &Hash::from([1u8; HASH_SIZE])
            )
            .matches(&pkg_def_with_variant)
                == false
        );
    }

    fn count_sources(components: HashMap<Url, Component>) -> (usize, usize, usize) {
        let mut zbi_bootfs_count = 0;
        let mut package_count = 0;
        let mut static_package_count = 0;
        for (_, comp) in components {
            match comp.source {
                ComponentSource::ZbiBootfs => {
                    zbi_bootfs_count += 1;
                }
                ComponentSource::Package(_) => {
                    package_count += 1;
                }
                ComponentSource::StaticPackage(_) => {
                    static_package_count += 1;
                }
            }
        }
        (zbi_bootfs_count, package_count, static_package_count)
    }

    #[fuchsia::test]
    fn test_extract_with_invalid_cm_paths_creates_empty_graph() {
        // Create a single test package with invalid cm paths
        let cms = create_test_cm_map(vec![
            (PathBuf::from("foo/bar.cm"), vec![0, 1]),
            (PathBuf::from("meta/baz"), vec![1, 0]),
        ]);
        let pkg = create_test_package_with_cms(PackageName::from_str("foo").unwrap(), None, cms);
        let served = vec![pkg];

        let services = HashMap::new();

        let mut artifact_loader: Box<dyn ArtifactReader> = Box::new(MockArtifactReader::new());
        let response = PackageDataCollector::extract(
            &empty_update_pkg(),
            &mut artifact_loader,
            served,
            services,
            &None,
            false,
        )
        .unwrap();

        assert_eq!(0, response.components.len());
        assert_eq!(0, response.manifests.len());
        assert_eq!(1, response.packages.len());
        assert_eq!(None, response.zbi);
        // 0 zbi/bootfs, 0 (non-static) package, 0 static packages.
        assert_eq!((0, 0, 0), count_sources(response.components));
    }

    #[fuchsia::test]
    fn test_extract_with_cm() {
        let cms = create_test_cm_map(vec![(PathBuf::from("meta/foo.cm"), vec![])]);
        let pkg = create_test_package_with_cms(PackageName::from_str("foo").unwrap(), None, cms);
        let served = vec![pkg];

        let services = HashMap::new();

        let mut artifact_loader: Box<dyn ArtifactReader> = Box::new(MockArtifactReader::new());
        let response = PackageDataCollector::extract(
            &empty_update_pkg(),
            &mut artifact_loader,
            served,
            services,
            &None,
            false,
        )
        .unwrap();

        assert_eq!(1, response.components.len());
        assert_eq!(1, response.manifests.len());
        assert_eq!(1, response.packages.len());
        assert_eq!(None, response.zbi);
    }

    #[fuchsia::test]
    fn test_collect_clears_data_model_before_adding_new() {
        let mut mock_pkg_reader = Box::new(MockPackageReader::new());
        let (_, model) = create_model();
        // Put some "previous" content into the model.
        {
            let mut comps = vec![];
            comps.push(Component {
                id: 1,
                url: Url::parse("fuchsia-pkg://test.fuchsia.com/test?hash=0000000000000000000000000000000000000000000000000000000000000000#meta/test.component.cm").unwrap(),
                source: ComponentSource::ZbiBootfs,
            });
            comps.push(Component {
                id: 1,
                url: Url::parse("fuchsia-pkg://test.fuchsia.com/test?hash=0000000000000000000000000000000000000000000000000000000000000000#meta/foo.bar.cm").unwrap(),
                source: fake_component_src_pkg(),
            });
            model.set(Components { entries: comps }).unwrap();

            let mut manis = vec![];
            manis.push(crate::core::collection::Manifest {
                component_id: 1,
                manifest: make_v2_manifest_data(ComponentDecl { ..ComponentDecl::default() }),
                uses: vec![Capability::Protocol(ProtocolCapability::new(String::from(
                    "test.service",
                )))],
            });
            manis.push(crate::core::collection::Manifest {
                component_id: 2,
                manifest: make_v2_manifest_data(ComponentDecl { ..ComponentDecl::default() }),
                uses: Vec::new(),
            });
            model.set(Manifests { entries: manis }).unwrap();
        }

        let cms = create_test_cm_map(vec![(PathBuf::from("meta/bar.cm"), vec![])]);
        let pkg = create_test_package_with_cms(PackageName::from_str("foo").unwrap(), None, cms);
        let pkg_urls = vec![pkg.url.clone().pinned().unwrap()];
        mock_pkg_reader.append_update_package(pkg_urls, empty_update_pkg());
        mock_pkg_reader.append_pkg_def(pkg);

        let mock_artifact_reader = Box::new(MockArtifactReader::new());
        PackageDataCollector::collect_with_reader(
            fake_model_config(),
            mock_pkg_reader,
            mock_artifact_reader,
            Arc::clone(&model),
        )
        .unwrap();

        // Ensure the model reflects only the latest collection.
        let comps = &model.get::<Components>().unwrap().entries;
        let manis = &model.get::<Manifests>().unwrap().entries;
        // There is 1 component, 1 manifest (for the defined package), and 0 routes
        assert_eq!(comps.len(), 1);
        assert_eq!(manis.len(), 1);
    }

    #[fuchsia::test]
    fn test_malformed_zbi() {
        let mut contents = HashMap::new();
        contents.insert(PathBuf::from("zbi"), Hash::from([0u8; HASH_SIZE]));
        let pkg = create_test_package_with_contents(
            PackageName::from_str("update").unwrap(),
            Some(PackageVariant::zero()),
            contents,
        );
        let served = vec![pkg];
        let services = HashMap::new();

        let mut artifact_loader: Box<dyn ArtifactReader> = Box::new(MockArtifactReader::new());
        let response = PackageDataCollector::extract(
            &empty_update_pkg(),
            &mut artifact_loader,
            served,
            services,
            &None,
            false,
        )
        .unwrap();
        assert_eq!(None, response.zbi);
    }

    #[fuchsia::test]
    fn test_packages_sorted() {
        let mut mock_pkg_reader = Box::new(MockPackageReader::new());
        let (_, model) = create_model();

        let cms_0 = create_test_cm_map(vec![(PathBuf::from("meta/foo.cm"), vec![])]);
        let pkg_0 =
            create_test_package_with_cms(PackageName::from_str("foo").unwrap(), None, cms_0);
        let pkg_0_url = pkg_0.url.clone();
        mock_pkg_reader.append_pkg_def(pkg_0);

        let cms_1 = create_test_cm_map(vec![(PathBuf::from("meta/bar.cm"), vec![])]);
        let pkg_1 =
            create_test_package_with_cms(PackageName::from_str("bar").unwrap(), None, cms_1);
        let pkg_1_url = pkg_1.url.clone();
        mock_pkg_reader.append_pkg_def(pkg_1);

        let pkg_urls = vec![pkg_0_url.pinned().unwrap(), pkg_1_url.pinned().unwrap()];
        mock_pkg_reader.append_update_package(pkg_urls, empty_update_pkg());

        let mock_artifact_reader = MockArtifactReader::new();
        let package_reader: Box<dyn PackageReader> = mock_pkg_reader;
        let artifact_reader: Box<dyn ArtifactReader> = Box::new(mock_artifact_reader);

        PackageDataCollector::collect_with_reader(
            fake_model_config(),
            package_reader,
            artifact_reader,
            Arc::clone(&model),
        )
        .unwrap();

        // Test that the packages are in sorted order.
        let packages = &model.get::<Packages>().unwrap().entries;
        assert_eq!(packages.len(), 2);
        assert_eq!(packages[0].name.to_string(), "bar");
        assert_eq!(packages[1].name.to_string(), "foo");
    }

    #[fuchsia::test]
    fn test_deps() {
        let mut mock_pkg_reader = Box::new(MockPackageReader::new());
        let (_, model) = create_model();

        let merkle_one = Hash::from([1u8; HASH_SIZE]);
        let merkle_two = Hash::from([2u8; HASH_SIZE]);
        let pkg_url_one = AbsolutePackageUrl::parse(&format!(
            "fuchsia-pkg://test.fuchsia.com/one-two-three?hash={}",
            merkle_one.to_string()
        ))
        .unwrap();
        let pkg_url_two = AbsolutePackageUrl::parse(&format!(
            "fuchsia-pkg://test.fuchsia.com/four-five-six?hash={}",
            merkle_two.to_string()
        ))
        .unwrap();

        let mut mock_artifact_reader = Box::new(MockArtifactReader::new());
        mock_pkg_reader.append_pkg_def(PackageDefinition {
            url: pkg_url_one.clone(),
            meta: hashmap! {},
            contents: hashmap! {},
            cms: hashmap! {},
            cvfs: hashmap! {},
        });
        mock_pkg_reader.append_pkg_def(PackageDefinition {
            url: pkg_url_two.clone(),
            meta: hashmap! {},
            contents: hashmap! {},
            cms: hashmap! {},
            cvfs: hashmap! {},
        });

        let pkg_urls = vec![pkg_url_one.pinned().unwrap(), pkg_url_two.pinned().unwrap()];
        mock_pkg_reader.append_update_package(pkg_urls, empty_update_pkg());

        // Append a dep to both package and artifact readers.
        let pkg_path: PathBuf = "pkg.far".to_string().into();
        let artifact_path: PathBuf = "artifact".to_string().into();
        mock_pkg_reader.append_dep(pkg_path.clone());
        mock_artifact_reader.append_dep(artifact_path.clone());

        PackageDataCollector::collect_with_reader(
            fake_model_config(),
            mock_pkg_reader,
            mock_artifact_reader,
            Arc::clone(&model),
        )
        .unwrap();
        let deps: Arc<CoreDataDeps> = model.get().unwrap();
        assert_eq!(
            deps,
            Arc::new(CoreDataDeps {
                deps: hashset! {
                    pkg_path,
                    artifact_path,
                },
            })
        );
    }

    #[fuchsia::test]
    fn test_missing_images_json_fuchsia_zbi() {
        let zbi_contents = zero_content_zbi();
        let zbi_hash = MerkleTree::from_reader(zbi_contents.as_slice()).unwrap().root();

        // Create valid images.json, but do not add any images to it. In particular, no "fuchsia"
        // image added (which code under test will look for).
        let images_json = ImagePackagesManifest::builder().build();
        let images_json_contents = serde_json::to_vec(&images_json).unwrap();
        let images_json_hash =
            MerkleTree::from_reader(images_json_contents.as_slice()).unwrap().root();

        let update_pkg = create_test_partial_package_with_contents(hashmap! {
            // ZBI is designated in update package as "zbi".
            "zbi".into() => zbi_hash.clone(),
            // Update package contains images.json defined above.
            "images.json".into() => images_json_hash.clone(),
        });

        let mut mock_artifact_reader = MockArtifactReader::new();

        // Code under test will read artifacts in the following order:
        // 1. images.json to determine its ZBI hash;
        mock_artifact_reader.append_artifact(&images_json_hash.to_string(), images_json_contents);
        // 2. the ZBI designated in update package.
        mock_artifact_reader.append_artifact(&zbi_hash.to_string(), zbi_contents);

        let mut artifact_reader: Box<dyn ArtifactReader> = Box::new(mock_artifact_reader);

        // Extraction should succeed because ZBI in update package is sufficient.
        let result = PackageDataCollector::extract_zbi_from_update_package(
            &mut artifact_reader,
            &update_pkg,
            &vec![],
            false,
        );
        match result {
            Ok(_) => return,
            Err(err) => panic!("Unexpected error: {:?}", err),
        };
    }

    #[fuchsia::test]
    fn test_missing_update_package_fuchsia_zbi() {
        let zbi_contents = zero_content_zbi();
        let zbi_content_hash = content_hash(zbi_contents.as_slice());
        let zbi_hash = MerkleTree::from_reader(zbi_contents.as_slice()).unwrap().root();

        // Create valid images.json with images that includes a ZBI.
        let mut images_json_builder = ImagePackagesManifest::builder();
        let url = "fuchsia-pkg://test.fuchsia.com/update-images-fuchsia/0?hash=0000000000000000000000000000000000000000000000000000000000000000#zbi".parse().unwrap();

        images_json_builder.fuchsia_package(
            ImageMetadata::new(zbi_contents.len().try_into().unwrap(), zbi_content_hash, url),
            None,
        );
        let images_json = images_json_builder.build();
        let images_json_contents = serde_json::to_vec(&images_json).unwrap();
        let images_json_hash =
            MerkleTree::from_reader(images_json_contents.as_slice()).unwrap().root();

        let update_pkg = create_test_partial_package_with_contents(
            // No ZBI designated in update package (either as "zbi" or "zbi.signed").
            hashmap! {
                // Update package contains images.json defined above.
                "images.json".into() => images_json_hash.clone(),
            },
        );
        let images_pkg = create_test_package_with_contents(
            PackageName::from_str("update-images-fuchsia").unwrap(),
            Some(PackageVariant::zero()),
            // Designate a ZBI in images package.
            hashmap! {
                "zbi".into() => zbi_hash.clone(),
            },
        );

        let mut mock_artifact_reader = MockArtifactReader::new();

        // Code under test will read artifacts in the following order:
        // 1. images.json to determine its ZBI hash;
        mock_artifact_reader.append_artifact(&images_json_hash.to_string(), images_json_contents);
        // 2. the ZBI designated in images.json.
        mock_artifact_reader.append_artifact(&zbi_hash.to_string(), zbi_contents);

        let mut artifact_reader: Box<dyn ArtifactReader> = Box::new(mock_artifact_reader);

        // Extraction should succeed because ZBI in images.json is sufficient.
        let result = PackageDataCollector::extract_zbi_from_update_package(
            &mut artifact_reader,
            &update_pkg,
            &vec![images_pkg],
            false,
        );

        match result {
            Ok(_) => return,
            Err(err) => panic!("Unexpected error: {:?}", err),
        };
    }

    #[fuchsia::test]
    fn test_read_recovery_zbi_with_two_zbis_available() {
        let fuchsia_zbi_path = "zbi";
        let recovery_zbi_path = "recovery";

        // Use `non_zero_content_zbi()` to construct fuchsia ZBI that is different from the recovery
        // ZBI that is expected to be read by code under test.
        let fuchsia_zbi_contents = non_zero_content_zbi();
        let fuchsia_zbi_content_hash = content_hash(fuchsia_zbi_contents.as_slice());
        let fuchsia_zbi_hash =
            MerkleTree::from_reader(fuchsia_zbi_contents.as_slice()).unwrap().root();

        // Use `zero_content_zbi()` for ZBI that is expected to be read without error.
        let recovery_zbi_contents = zero_content_zbi();
        let recovery_zbi_content_hash = content_hash(recovery_zbi_contents.as_slice());
        let recovery_zbi_hash =
            MerkleTree::from_reader(recovery_zbi_contents.as_slice()).unwrap().root();

        // Create valid images.json with images that includes both ZBIs.
        let mut images_json_builder = ImagePackagesManifest::builder();
        let update_images_pkg_url_str = "fuchsia-pkg://test.fuchsia.com/update-images-fuchsia/0?hash=0000000000000000000000000000000000000000000000000000000000000000";
        let fuchsia_zbi_url: AbsoluteComponentUrl =
            format!("{}#{}", update_images_pkg_url_str, fuchsia_zbi_path).parse().unwrap();
        let recovery_zbi_url: AbsoluteComponentUrl =
            format!("{}#{}", update_images_pkg_url_str, recovery_zbi_path).parse().unwrap();

        images_json_builder.fuchsia_package(
            ImageMetadata::new(
                fuchsia_zbi_contents.len().try_into().unwrap(),
                fuchsia_zbi_content_hash,
                fuchsia_zbi_url,
            ),
            None,
        );
        images_json_builder.recovery_package(
            ImageMetadata::new(
                recovery_zbi_contents.len().try_into().unwrap(),
                recovery_zbi_content_hash,
                recovery_zbi_url,
            ),
            None,
        );
        let images_json = images_json_builder.build();
        let images_json_contents = serde_json::to_vec(&images_json).unwrap();
        let images_json_hash =
            MerkleTree::from_reader(images_json_contents.as_slice()).unwrap().root();

        let update_pkg = create_test_partial_package_with_contents(hashmap! {
            // Update package contains images.json and both fuchsia and recovery ZBIs.
            "images.json".into() => images_json_hash.clone(),
            fuchsia_zbi_path.into() => fuchsia_zbi_hash.clone(),
            recovery_zbi_path.into() => recovery_zbi_hash.clone(),
        });
        let images_pkg = create_test_package_with_contents(
            PackageName::from_str("update-images-fuchsia").unwrap(),
            Some(PackageVariant::zero()),
            // Designate ZBIs in images package.
            hashmap! {
                fuchsia_zbi_path.into() => fuchsia_zbi_hash.clone(),
                recovery_zbi_path.into() => recovery_zbi_hash.clone(),
            },
        );

        let mut mock_artifact_reader = MockArtifactReader::new();

        // Code under test will read artifacts in the following order:
        // 1. images.json to determine its ZBI hash;
        mock_artifact_reader.append_artifact(&images_json_hash.to_string(), images_json_contents);
        // 2. the recovery ZBI designated in images.json.
        mock_artifact_reader.append_artifact(&recovery_zbi_hash.to_string(), recovery_zbi_contents);

        let mut artifact_reader: Box<dyn ArtifactReader> = Box::new(mock_artifact_reader);

        // Extraction should succeed because ZBI in images.json is sufficient.
        let result = PackageDataCollector::extract_zbi_from_update_package(
            &mut artifact_reader,
            &update_pkg,
            &vec![images_pkg],
            true,
        );

        match result {
            Ok(_) => return,
            Err(err) => panic!("Unexpected error: {:?}", err),
        };
    }

    #[fuchsia::test]
    fn test_read_fuchsia_zbi_with_two_zbis_available() {
        let fuchsia_zbi_path = "zbi";
        let recovery_zbi_path = "recovery";

        // Use `zero_content_zbi()` for ZBI that is expected to be read without error.
        let fuchsia_zbi_contents = zero_content_zbi();
        let fuchsia_zbi_content_hash = content_hash(fuchsia_zbi_contents.as_slice());
        let fuchsia_zbi_hash =
            MerkleTree::from_reader(fuchsia_zbi_contents.as_slice()).unwrap().root();

        // Use `non_zero_content_zbi()` to construct fuchsia ZBI that is different from the fuchsia
        // ZBI that is expected to be read by code under test.
        let recovery_zbi_contents = non_zero_content_zbi();
        let recovery_zbi_content_hash = content_hash(recovery_zbi_contents.as_slice());
        let recovery_zbi_hash =
            MerkleTree::from_reader(recovery_zbi_contents.as_slice()).unwrap().root();

        // Create valid images.json with images that includes both ZBIs.
        let mut images_json_builder = ImagePackagesManifest::builder();
        let update_images_pkg_url_str = "fuchsia-pkg://test.fuchsia.com/update-images-fuchsia/0?hash=0000000000000000000000000000000000000000000000000000000000000000";
        let fuchsia_zbi_url: AbsoluteComponentUrl =
            format!("{}#{}", update_images_pkg_url_str, fuchsia_zbi_path).parse().unwrap();
        let recovery_zbi_url: AbsoluteComponentUrl =
            format!("{}#{}", update_images_pkg_url_str, recovery_zbi_path).parse().unwrap();

        images_json_builder.fuchsia_package(
            ImageMetadata::new(
                fuchsia_zbi_contents.len().try_into().unwrap(),
                fuchsia_zbi_content_hash,
                fuchsia_zbi_url,
            ),
            None,
        );
        images_json_builder.recovery_package(
            ImageMetadata::new(
                recovery_zbi_contents.len().try_into().unwrap(),
                recovery_zbi_content_hash,
                recovery_zbi_url,
            ),
            None,
        );
        let images_json = images_json_builder.build();
        let images_json_contents = serde_json::to_vec(&images_json).unwrap();
        let images_json_hash =
            MerkleTree::from_reader(images_json_contents.as_slice()).unwrap().root();

        let update_pkg = create_test_partial_package_with_contents(hashmap! {
            // Update package contains images.json and both fuchsia and recovery ZBIs.
            "images.json".into() => images_json_hash.clone(),
            fuchsia_zbi_path.into() => fuchsia_zbi_hash.clone(),
            recovery_zbi_path.into() => recovery_zbi_hash.clone(),
        });
        let images_pkg = create_test_package_with_contents(
            PackageName::from_str("update-images-fuchsia").unwrap(),
            Some(PackageVariant::zero()),
            // Designate ZBIs in images package.
            hashmap! {
                fuchsia_zbi_path.into() => fuchsia_zbi_hash.clone(),
                recovery_zbi_path.into() => recovery_zbi_hash.clone(),
            },
        );

        let mut mock_artifact_reader = MockArtifactReader::new();

        // Code under test will read artifacts in the following order:
        // 1. images.json to determine its ZBI hash;
        mock_artifact_reader.append_artifact(&images_json_hash.to_string(), images_json_contents);
        // 2. the fuchsia ZBI designated in images.json.
        mock_artifact_reader.append_artifact(&fuchsia_zbi_hash.to_string(), fuchsia_zbi_contents);

        let mut artifact_reader: Box<dyn ArtifactReader> = Box::new(mock_artifact_reader);

        // Extraction should succeed because ZBI in images.json is sufficient.
        let result = PackageDataCollector::extract_zbi_from_update_package(
            &mut artifact_reader,
            &update_pkg,
            &vec![images_pkg],
            false,
        );

        match result {
            Ok(_) => return,
            Err(err) => panic!("Unexpected error: {:?}", err),
        };
    }

    #[fuchsia::test]
    fn test_update_package_vs_images_json_zbi_hash_mismatch() {
        let zbi_contents = zero_content_zbi();
        let zbi_content_hash = content_hash(zbi_contents.as_slice());
        let zbi_hash = MerkleTree::from_reader(zbi_contents.as_slice()).unwrap().root();
        let zbi_mismatch_hash = Hash::from([9; HASH_SIZE]);
        assert!(zbi_hash != zbi_mismatch_hash);

        // Create valid images.json with "fuchsia" images that includes a ZBI.
        let mut images_json_builder = ImagePackagesManifest::builder();
        let url = "fuchsia-pkg://test.fuchsia.com/update-images-fuchsia/0?hash=0000000000000000000000000000000000000000000000000000000000000000#zbi".parse().unwrap();

        images_json_builder.fuchsia_package(
            ImageMetadata::new(zbi_contents.len().try_into().unwrap(), zbi_content_hash, url),
            None,
        );

        let images_json = images_json_builder.build();
        let images_json_contents = serde_json::to_vec(&images_json).unwrap();
        let images_json_hash =
            MerkleTree::from_reader(images_json_contents.as_slice()).unwrap().root();

        let update_pkg = create_test_partial_package_with_contents(
            // ZBI is designated in update package as "zbi". Note that fake hash string,
            // "zbi_hash_from_update_package" will not match string for hash
            // "zbi_hash_from_images_package" designated by `images_pkg` below.
            hashmap! {
                "zbi".into() => zbi_hash,
                // Update package contains images.json defined above.
                "images.json".into() => images_json_hash.clone(),
            },
        );
        let images_pkg = create_test_package_with_contents(
            PackageName::from_str("update-images-fuchsia").unwrap(),
            Some(PackageVariant::zero()),
            // Designate a ZBI in images package.
            hashmap! {
                "zbi".into() => zbi_mismatch_hash,
            },
        );

        let mut mock_artifact_reader = MockArtifactReader::new();

        // Code under test will read one artifact: images.json.
        mock_artifact_reader.append_artifact(
            &images_json_hash.to_string(),
            serde_json::to_vec(&images_json).unwrap(),
        );

        let mut artifact_reader: Box<dyn ArtifactReader> = Box::new(mock_artifact_reader);
        assert!(PackageDataCollector::extract_zbi_from_update_package(
            &mut artifact_reader,
            &update_pkg,
            &vec![images_pkg],
            false,
        )
        .err()
        .unwrap()
        .to_string()
        .contains("Update package and its images manifest contain different fuchsia ZBI images"));
    }

    #[fuchsia::test]
    fn test_update_package_vs_images_json_zbi_hash_match() {
        let zbi_contents = zero_content_zbi();
        let zbi_content_hash = content_hash(zbi_contents.as_slice());
        let zbi_hash = MerkleTree::from_reader(zbi_contents.as_slice()).unwrap().root();

        // Create valid images.json with "fuchsia" images that includes a ZBI.
        let mut images_json_builder = ImagePackagesManifest::builder();
        let url = "fuchsia-pkg://fuchsia.com/update-images-firmware/0?hash=000000000000000000000000000000000000000000000000000000000000000a#zbi".parse().unwrap();

        images_json_builder.fuchsia_package(
            ImageMetadata::new(zbi_contents.len().try_into().unwrap(), zbi_content_hash, url),
            None,
        );
        let images_json = images_json_builder.build();
        let images_json_contents = serde_json::to_vec(&images_json).unwrap();
        let images_json_hash =
            MerkleTree::from_reader(images_json_contents.as_slice()).unwrap().root();

        let update_pkg = create_test_partial_package_with_contents(hashmap! {
            // ZBI is designated in update package as "zbi". Hash string value matches hash in
            // `images_json` above.
            "zbi".into() => zbi_hash.clone(),
            // Update package contains images.json defined above.
            "images.json".into() => images_json_hash.clone(),
        });
        let images_pkg = create_test_package_with_contents(
            PackageName::from_str("update-images-fuchsia").unwrap(),
            Some(PackageVariant::zero()),
            // Designate a ZBI in images package.
            hashmap! {
                "zbi".into() => zbi_hash.clone(),
            },
        );

        let mut mock_artifact_reader = MockArtifactReader::new();

        // Code under test will read artifacts in the following order:
        // 1. images.json to determine its ZBI hash;
        mock_artifact_reader.append_artifact(&images_json_hash.to_string(), images_json_contents);
        // 2. the ZBI designated by both the update package and image.json.
        mock_artifact_reader.append_artifact(&zbi_hash.to_string(), zbi_contents);

        let mut artifact_reader: Box<dyn ArtifactReader> = Box::new(mock_artifact_reader);
        let result = PackageDataCollector::extract_zbi_from_update_package(
            &mut artifact_reader,
            &update_pkg,
            &vec![images_pkg],
            false,
        );
        match result {
            Ok(_) => return,
            Err(err) => panic!("Unexpected error: {:?}", err),
        };
    }
}
