// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Result};
use argh::FromArgs;
use camino::Utf8PathBuf;
use ext4_extract::ext4_extract;
use fuchsia_pkg::{PackageBuilder, PackageManifest};
use fuchsia_url::RelativePackageUrl;

/// Construct a starnix container that can include an Android system and HALs.
#[derive(FromArgs)]
struct Command {
    /// name of the starnix container.
    #[argh(option)]
    name: String,

    /// directory to place outputs into.
    #[argh(option)]
    outdir: Utf8PathBuf,

    /// path to package archive containing additional resources to include.
    #[argh(option)]
    base: Utf8PathBuf,

    /// path to an Android system image.
    #[argh(option)]
    system: Utf8PathBuf,

    /// path to hal package archive.
    #[argh(option)]
    hal: Vec<Utf8PathBuf>,

    /// path to a depfile to write.
    #[argh(option)]
    depfile: Option<Utf8PathBuf>,
}

fn main() -> Result<()> {
    let cmd: Command = argh::from_env();
    generate(cmd)
}

fn generate(cmd: Command) -> Result<()> {
    // Bootstrap the package builder with the contents of the base package, but update the
    // internal and published names.
    let manifest = PackageManifest::try_load_from(&cmd.base)
        .with_context(|| format!("Reading base starnix package: {}", cmd.base))?;
    let mut builder = PackageBuilder::from_manifest(manifest, &cmd.outdir)
        .context("Parsing base starnix package")?;
    builder.name(&cmd.name);
    builder.published_name(&cmd.name);

    // Track inputs for producing a depfile for incremental build correctness.
    let mut inputs_for_depfile: Vec<String> = Vec::new();

    // Add all the HALs as subpackages.
    for hal in &cmd.hal {
        let manifest = PackageManifest::try_load_from(&hal)
            .with_context(|| format!("Reading hal package manifest: {}", hal))?;
        let name: RelativePackageUrl = manifest.name().to_owned().into();
        builder
            .add_subpackage(&name, manifest.hash(), hal.into())
            .with_context(|| format!("Adding subpackage from manifest: {}", &hal))?;

        // If a HAL package contains `system/init.rc`, copy that file to
        // `data/init/${hal_package_name}.rc` in the resulting container package.
        const HAL_INIT_RC_PATH: &str = "system/init.rc";
        if let Some(init_rc_blob) =
            manifest.blobs().iter().find(|blob| blob.path == HAL_INIT_RC_PATH)
        {
            let hal_package_name = manifest.name().to_string();
            let src = &init_rc_blob.source_path;
            let dst = format!("data/init/{hal_package_name}.rc");
            builder.add_file_as_blob(&dst, src).with_context(|| {
                format!("Adding init.rc for {hal_package_name} from {src} to {dst}")
            })?;
            inputs_for_depfile.push(src.clone());
        }
    }

    // Put all the system image files into the container.
    let system_outdir = cmd.outdir.join("system");
    std::fs::create_dir_all(&system_outdir)
        .with_context(|| format!("Preparing directory for system files: {}", &system_outdir))?;
    let system_files = ext4_extract(cmd.system.as_str(), system_outdir.as_str())
        .context("Extracting system files")?;
    for (dst, src) in &system_files {
        let dst = format!("data/system/{}", dst);
        builder
            .add_file_as_blob(dst, &src)
            .with_context(|| format!("Adding blob from file: {}", &src))?;
        inputs_for_depfile.push(src.clone());
    }

    // Build the starnix container.
    let metafar_path = cmd.outdir.join("meta.far");
    let manifest_path = cmd.outdir.join("package_manifest.json");
    builder.manifest_path(manifest_path);
    builder.build(&cmd.outdir, &metafar_path).context("Building starnix container")?;

    if let Some(depfile) = cmd.depfile {
        let mut deps: Vec<String> = vec![
            cmd.outdir.join("meta.far"),
            cmd.outdir.join("meta/fuchsia.abi/abi-revision"),
            cmd.outdir.join("meta/fuchsia.pkg/subpackages"),
            cmd.outdir.join("meta/package"),
            cmd.outdir.join(format!("meta/{}.cm", cmd.name)),
        ]
        .iter()
        .map(|p| p.to_string())
        .collect();
        deps.extend(inputs_for_depfile);
        let mut deps: String = deps.iter().map(|p| format!("{p} ")).collect();
        deps += &format!(": {}", cmd.system);
        std::fs::write(&depfile, &deps).with_context(|| format!("Writing depfile: {}", depfile))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8Path;
    use std::str::FromStr;
    use tempfile::TempDir;

    const EXT4_IMAGE_PATH: &str = "host_x64/test_data/gen-android-starnix-container/test.img";

    fn fake_base(outdir: &Utf8Path) -> Utf8PathBuf {
        // Build a fake "base".
        let base_manifest_path = outdir.join("base_package_manifest.json");
        let mut builder = PackageBuilder::new("test-base");
        builder.add_contents_as_blob("data/test", "test-base-blob", &outdir).unwrap();
        builder.manifest_path(&base_manifest_path);
        let _ = builder.build(&outdir, outdir.join("base-meta.far")).unwrap();
        base_manifest_path
    }

    #[test]
    fn test_generate() {
        let tmp = TempDir::new().unwrap();
        let outdir = Utf8Path::from_path(tmp.path()).unwrap();
        let base_manifest_path = fake_base(outdir);

        // Build a fake HAL.
        let hal_manifest_path = outdir.join("hal_package_manifest.json");
        let mut builder = PackageBuilder::new("test-hal");
        builder.add_contents_as_blob("data/hal", "test-hal-blob", &outdir).unwrap();
        builder.manifest_path(&hal_manifest_path);
        let _ = builder.build(&outdir, outdir.join("hal-meta.far")).unwrap();

        // Run the generator.
        let cmd = Command {
            name: "test-name".into(),
            outdir: outdir.to_owned(),
            base: base_manifest_path,
            system: Utf8PathBuf::from_str(EXT4_IMAGE_PATH).unwrap(),
            hal: vec![hal_manifest_path],
            depfile: None,
        };
        generate(cmd).unwrap();

        // Read the package manifest, and ensure the correct files are present as blobs, and the
        // HALs are listed as subpackages.
        let manifest_path = outdir.join("package_manifest.json");
        let manifest = PackageManifest::try_load_from(manifest_path).unwrap();
        assert_eq!(manifest.name().as_ref(), "test-name");
        let (blobs, subpackages) = manifest.into_blobs_and_subpackages();
        assert_eq!(blobs.len(), 4);
        assert_eq!(subpackages.len(), 1);
        let blob_filenames: Vec<String> = blobs.into_iter().map(|b| b.path).collect();
        let subpackage_names: Vec<String> = subpackages.into_iter().map(|s| s.name).collect();
        assert_eq!(
            blob_filenames,
            vec![
                "meta/".to_string(),
                "data/system/13".to_string(),
                "data/system/metadata.v1".to_string(),
                "data/test".to_string(),
            ]
        );
        assert_eq!(subpackage_names, vec!["test-hal".to_string(),]);
    }

    #[test]
    fn test_hal_init_rc() {
        let tmp = TempDir::new().unwrap();
        let outdir = Utf8Path::from_path(tmp.path()).unwrap();
        let base_manifest_path = fake_base(outdir);

        // Build a fake HAL with an init.rc file.
        let hal_manifest_path = outdir.join("hal_package_manifest.json");
        let mut builder = PackageBuilder::new("test-hal");
        builder.add_contents_as_blob("data/hal", "test-hal-blob", &outdir).unwrap();
        builder.add_contents_as_blob("system/init.rc", "service foo bar", &outdir).unwrap();
        builder.manifest_path(&hal_manifest_path);
        let _ = builder.build(&outdir, outdir.join("hal-meta.far")).unwrap();

        // Run the generator.
        let cmd = Command {
            name: "test-name".into(),
            outdir: outdir.to_owned(),
            base: base_manifest_path,
            system: Utf8PathBuf::from_str(EXT4_IMAGE_PATH).unwrap(),
            hal: vec![hal_manifest_path],
            depfile: None,
        };
        generate(cmd).unwrap();

        // Read the package manifest, and ensure the correct files are present as blobs, and
        // there is an additional `.rc` file corresponding to `test-hal`.
        let manifest_path = outdir.join("package_manifest.json");
        let manifest = PackageManifest::try_load_from(manifest_path).unwrap();
        assert_eq!(manifest.name().as_ref(), "test-name");
        let (blobs, _subpackages) = manifest.into_blobs_and_subpackages();
        assert_eq!(blobs.len(), 5);
        let blob_filenames: Vec<String> = blobs.into_iter().map(|b| b.path).collect();
        assert_eq!(
            blob_filenames,
            vec![
                "meta/".to_string(),
                "data/init/test-hal.rc".to_string(),
                "data/system/13".to_string(),
                "data/system/metadata.v1".to_string(),
                "data/test".to_string(),
            ]
        );
    }
}
