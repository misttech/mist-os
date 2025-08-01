// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod assembly;

use anyhow::{bail, Context, Result};
use assembly::Assembly;
use assembly_artifact_cache::ArtifactCache;
use assembly_tool::PlatformToolProvider;
use camino::Utf8PathBuf;
use delivery_blob::DeliveryBlobType;
use errors::FfxError;
use ffx_config::EnvironmentContext;
use ffx_product_create_args::CreateCommand;
use ffx_writer::{SimpleWriter, ToolIO};
use fho::{FfxMain, FfxTool};
use product_bundle::{ProductBundleBuilder, Slot};
use tempfile::tempdir;

#[derive(FfxTool)]
pub struct ProductBundleCreateTool {
    #[command]
    pub cmd: CreateCommand,
    ctx: EnvironmentContext,
}

fho::embedded_plugin!(ProductBundleCreateTool);

/// Create a fuchsia product bundle.
#[async_trait::async_trait(?Send)]
impl FfxMain for ProductBundleCreateTool {
    type Writer = SimpleWriter;
    async fn main(self, writer: Self::Writer) -> fho::Result<()> {
        let build_dir = self
            .ctx
            .build_dir()
            .map(|p| {
                Utf8PathBuf::from_path_buf(p.to_path_buf()).map_err(|build_dir| {
                    fho::bug!("Failed to parse build_dir as utf8: {}", build_dir.display())
                })
            })
            .transpose()?;
        product_bundle_create(self.cmd, build_dir, writer).await.map_err(flatten_error_sources)
    }
}

/// Create a fuchsia product bundle and return an anyhow result.
/// This allows us to work with anyhow, and map to a fho result above.
async fn product_bundle_create(
    cmd: CreateCommand,
    build_dir: Option<Utf8PathBuf>,
    writer: SimpleWriter,
) -> Result<()> {
    let sanitized_cmd = cmd.try_into()?;
    Box::pin(sanitized_product_bundle_create(sanitized_cmd, build_dir, writer)).await
}

/// Convert the anyhow error into a pretty/stacked fho error.
fn flatten_error_sources(e: anyhow::Error) -> fho::Error {
    FfxError::Error(
        anyhow::anyhow!(
            "Failed: {}{}",
            e,
            e.chain()
                .skip(1)
                .enumerate()
                .map(|(i, e)| format!("\n  {: >3}.  {}", i + 1, e))
                .collect::<Vec<String>>()
                .concat()
        ),
        -1,
    )
    .into()
}

/// All the inputs necessary to run `ffx product-bundle create` after checking
/// that the arguments were properly given.
struct SanitizedCreateCommand {
    /// The platform artifacts to use.
    /// If None, then we use the default local artifacts.
    pub platform: Option<String>,

    /// The product config to use.
    pub product_config: String,

    /// The board config to use.
    pub board_config: String,

    /// The version of the product to use.
    pub version: Option<String>,

    /// The tuf keys to use.
    pub tuf_keys: Option<Utf8PathBuf>,

    /// What result we want from running `ffx product create`.
    pub result: CreateResult,
}

/// What result we want from running `ffx product create`.
enum CreateResult {
    /// Stage the inputs, but do not run assembly.
    Stage,

    /// Run assembly and generate the outputs to this path.
    Out(Utf8PathBuf),
}

/// Parse the input command, and ensure that the user formatted it correctly,
/// then return a new command with all the requirements for running
/// `ffx product create`.
impl TryFrom<CreateCommand> for SanitizedCreateCommand {
    type Error = anyhow::Error;

    fn try_from(cmd: CreateCommand) -> Result<Self> {
        let platform = cmd.platform;
        let result = match (cmd.stage, cmd.out) {
            (true, _) => CreateResult::Stage,
            (false, Some(out)) => CreateResult::Out(out),
            (false, None) => bail!("--stage or --out must be supplied"),
        };

        // Choose between a product_config.board_config combo and --product --board flags.
        let (product_config, board_config) =
            if let Some(combo) = cmd.product_config_board_config_combo {
                let (p, b) = combo
                    .split_once(".")
                    .context("product_config.board_config combo must have a period")?;
                (p.to_string(), b.to_string())
            } else {
                let p = cmd.product_config.context(
                "--product-config must be supplied when product_config.board_config combo is not",
            )?;
                let b = cmd.board_config.context(
                    "--board-config must be supplied when product_config.board_config combo is not",
                )?;
                (p, b)
            };

        let version = cmd.version;
        let tuf_keys = cmd.tuf_keys;
        Ok(Self { platform, product_config, board_config, version, tuf_keys, result })
    }
}

/// Construct a product bundle using sanitized inputs.
async fn sanitized_product_bundle_create(
    cmd: SanitizedCreateCommand,
    build_dir: Option<Utf8PathBuf>,
    mut writer: SimpleWriter,
) -> Result<()> {
    let tmp = tempdir().unwrap();
    let tmp_path = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();

    let cache = ArtifactCache::new();
    let assembly =
        Assembly::new(&cache, cmd.platform, cmd.product_config, cmd.board_config, build_dir)?;
    writer.line(format!("Staged the artifacts\n{}", assembly.version_string()))?;

    // Return early if we are only staging the inputs.
    let out = match cmd.result {
        CreateResult::Stage => return Ok(()),
        CreateResult::Out(out) => out,
    };

    let product_name = assembly.product_config.product.release_info.info.name.clone();
    let board_name = assembly.board_config.release_info.info.name.clone();
    let name = format!("{}.{}", product_name, board_name);

    let version = cmd
        .version
        .unwrap_or_else(|| assembly.product_config.product.release_info.info.version.clone());
    let update_version_file = tmp_path.join("update_version.txt");
    std::fs::write(&update_version_file, &version)?;

    writer.line(format!("Assembling into {} ...", &out))?;
    let tools = PlatformToolProvider::new(assembly.platform_path.clone());
    let system = Box::pin(assembly.create_system(&tmp_path)).await?;
    let mut builder = ProductBundleBuilder::new(name, version)
        .system(system, Slot::A)
        .update_package(update_version_file, 1);

    if let Some(tuf_keys) = cmd.tuf_keys {
        builder = builder.repository(DeliveryBlobType::Type1, tuf_keys);
    }
    let _ = builder.build(Box::new(tools), out).await?;
    cache.purge()?;
    Ok(())
}
