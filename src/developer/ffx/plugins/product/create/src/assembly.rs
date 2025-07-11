// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Result};
use assembled_system::AssembledSystem;
use assembly_artifact_cache::{Artifact, ArtifactCache};
use assembly_config_schema::{AssemblyConfig, BoardInformation};
use assembly_container::AssemblyContainer;
use assembly_platform_artifacts::PlatformArtifacts;
use assembly_tool::PlatformToolProvider;
use camino::Utf8PathBuf;
use image_assembly_config_builder::{ProductAssembly, ValidationMode};

pub struct Assembly {
    pub platform_path: Utf8PathBuf,
    pub platform: PlatformArtifacts,
    pub product: AssemblyConfig,
    pub board: BoardInformation,
}

impl Assembly {
    pub fn new(
        cache: &ArtifactCache,
        platform: Option<String>,
        product: String,
        board: String,
        build_dir: Option<Utf8PathBuf>,
    ) -> Result<Self> {
        let product_artifact =
            Artifact::from_product_or_board_string(&product).context("Parsing product input");
        let product_artifact = product_artifact
            .or_else(|_| Artifact::from_local_product_name(&product, build_dir.as_ref()))
            .context("Parsing product as local build api")?;
        let product_path = cache.resolve(&product_artifact).context("Resolving product")?;
        let product = AssemblyConfig::from_dir(&product_path).context("Reading product")?;

        let board_artifact =
            Artifact::from_product_or_board_string(&board).context("Parsing board input");
        let board_artifact = board_artifact
            .or_else(|_| Artifact::from_local_board_name(&board, build_dir.as_ref()))
            .context("Parsing board as local build api")?;
        let board_path = cache.resolve(&board_artifact).context("Resolving board")?;
        let board = BoardInformation::from_dir(&board_path).context("Reading board")?;

        let platform_artifact = Artifact::from_platform(platform, &board.arch, build_dir.as_ref())?;
        let platform_path = cache.resolve(&platform_artifact).context("Resolving platform")?;
        let platform =
            PlatformArtifacts::from_dir_with_path(&platform_path).context("Reading platform")?;

        Ok(Self { platform_path, platform, product, board })
    }

    pub fn version_string(&self) -> String {
        format!(
            "\tplatform: {}@{}\n\tproduct: {}@{}\n\tboard: {}@{}",
            self.platform.release_info.name,
            self.platform.release_info.version,
            self.product.product.release_info.info.name,
            self.product.product.release_info.info.version,
            self.board.release_info.info.name,
            self.board.release_info.info.version,
        )
    }

    pub async fn create_system(self, outdir: &Utf8PathBuf) -> Result<AssembledSystem> {
        let should_configure_example =
            ffx_config::get::<bool, _>("assembly_example_enabled").unwrap_or_default();

        let tools = PlatformToolProvider::new(self.platform_path.clone());
        let mut product_assembly = ProductAssembly::new(
            self.platform,
            self.product,
            self.board,
            should_configure_example,
        )?;
        product_assembly.set_validation_mode(ValidationMode::Off);
        let iac = product_assembly.build(&tools, outdir)?;
        AssembledSystem::new(iac, false, outdir, &tools, None).await
    }
}
