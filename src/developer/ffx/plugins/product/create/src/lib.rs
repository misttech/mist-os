// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod assembly;

use anyhow::{bail, Context, Result};
use assembly::Assembly;
use assembly_artifact_cache::ArtifactCache;
use assembly_tool::PlatformToolProvider;
use camino::Utf8PathBuf;
use errors::FfxError;
use ffx_config::EnvironmentContext;
use ffx_product_create_args::CreateCommand;
use ffx_writer::{SimpleWriter, ToolIO};
use fho::{FfxMain, FfxTool};
use product_bundle::{ProductBundleBuilder, Slot};
use tempfile::tempdir;

#[derive(FfxTool)]
pub struct ProductCreateTool {
    #[command]
    pub cmd: CreateCommand,
    ctx: EnvironmentContext,
}

fho::embedded_plugin!(ProductCreateTool);

/// Create a fuchsia product bundle.
#[async_trait::async_trait(?Send)]
impl FfxMain for ProductCreateTool {
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
        product_create(self.cmd, build_dir, writer).await.map_err(flatten_error_sources)
    }
}

/// Create a fuchsia product bundle and return an anyhow result.
/// This allows us to work with anyhow, and map to a fho result above.
async fn product_create(
    cmd: CreateCommand,
    build_dir: Option<Utf8PathBuf>,
    writer: SimpleWriter,
) -> Result<()> {
    let sanitized_cmd = cmd.try_into()?;
    Box::pin(sanitized_product_create(sanitized_cmd, build_dir, writer)).await
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

/// All the inputs necessary to run `ffx product create` after checking that
/// the arguments were properly given.
struct SanitizedCreateCommand {
    /// The platform artifacts to use.
    /// If None, then we use the default local artifacts.
    pub platform: Option<String>,

    /// The product config to use.
    pub product: String,

    /// The board config to use.
    pub board: String,

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

        // Choose between a product.board combo and --product --board flags.
        let (product, board) = if let Some(combo) = cmd.product_board_combo {
            let (p, b) = combo.split_once(".").context("product.board combo must have a period")?;
            (p.to_string(), b.to_string())
        } else {
            let p = cmd
                .product
                .context("--product must be supplied when product.board combo is not")?;
            let b =
                cmd.board.context("--board must be supplied when product.board combo is not")?;
            (p, b)
        };

        Ok(Self { platform, product, board, result })
    }
}

/// Construct a product using sanitized inputs.
async fn sanitized_product_create(
    cmd: SanitizedCreateCommand,
    build_dir: Option<Utf8PathBuf>,
    mut writer: SimpleWriter,
) -> Result<()> {
    let tmp = tempdir().unwrap();
    let tmp_path = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();

    let cache = ArtifactCache::new();
    let assembly = Assembly::new(&cache, cmd.platform, cmd.product, cmd.board, build_dir)?;
    writer.line(format!("Staged the artifacts\n{}", assembly.version_string()))?;

    // Return early if we are only staging the inputs.
    let out = match cmd.result {
        CreateResult::Stage => return Ok(()),
        CreateResult::Out(out) => out,
    };

    writer.line(format!("Assembling into {} ...", &out))?;
    let tools = PlatformToolProvider::new(assembly.platform_path.clone());
    let system = Box::pin(assembly.create_system(&tmp_path)).await?;
    let _ = ProductBundleBuilder::new("my_pb", "testing")
        .system(system, Slot::A)
        .build(Box::new(tools), out)
        .await?;
    cache.purge()?;
    Ok(())
}
