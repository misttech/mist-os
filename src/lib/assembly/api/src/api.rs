// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use assembly_cli_args::{
    CreateSystemArgs, CreateSystemOutputs, ProductArgs, ProductAssemblyOutputs,
};
use assembly_tool::{PlatformToolProvider, ToolProvider};
use camino::Utf8PathBuf;

pub fn product_assembly(args: ProductArgs) -> Result<ProductAssemblyOutputs> {
    let tools = PlatformToolProvider::new(args.input_bundles_dir.clone());
    let assembly_tool = tools.get_tool("assembly")?;
    assembly_tool.run(&args.to_vec())?;

    let outputs = ProductAssemblyOutputs::from(args);
    Ok(outputs)
}

pub fn create_system(args: CreateSystemArgs) -> Result<CreateSystemOutputs> {
    let tools = PlatformToolProvider::new(args.platform.clone());
    let assembly_tool = tools.get_tool("assembly")?;
    assembly_tool.run(&args.to_vec())?;

    let outputs = CreateSystemOutputs::from(args);
    Ok(outputs)
}

pub fn assemble(args: ProductArgs) -> Result<CreateSystemOutputs> {
    // Create a temporary directory for the product assembly outputs.
    // We cannot use the directories in `args`, because those are reserved for
    // the system.
    let product_tmp = tempfile::TempDir::new().unwrap();
    let product_tmp = Utf8PathBuf::from_path_buf(product_tmp.path().to_path_buf()).unwrap();
    let product_out = product_tmp.join("out");
    let product_gen = product_tmp.join("gen");
    let product_args = ProductArgs { outdir: product_out, gendir: product_gen, ..args };
    let product_outputs = product_assembly(product_args)?;

    // The system is written to the outdir/gendir passed in with `args`.
    let create_system_args =
        CreateSystemArgs { outdir: args.outdir, gendir: args.gendir, ..product_outputs.into() };
    create_system(create_system_args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use assembly_tool::testing::FakeToolProvider;
    use assembly_tool::ToolCommandLog;
    use camino::Utf8PathBuf;
    use serde_json::json;

    #[test]
    fn product_assembly() {
        let tools = FakeToolProvider::default();
        let assembly_tool = tools.get_tool("assembly").unwrap();

        let product = Utf8PathBuf::from("path/to/product");
        let board_config = Utf8PathBuf::from("path/to/board_config");
        let outdir = Utf8PathBuf::from("path/to/outdir");
        let gendir = Utf8PathBuf::from("path/to/gendir");
        let input_bundles_dir = Utf8PathBuf::from("path/to/bundles");

        let args = ProductArgs {
            product: product.clone(),
            board_config: board_config.clone(),
            outdir: outdir.clone(),
            gendir: gendir.clone(),
            input_bundles_dir: input_bundles_dir.clone(),
            package_validation: None,
            custom_kernel_aib: None,
            custom_boot_shim_aib: None,
            suppress_overrides_warning: false,
            developer_overrides: None,
            include_example_aib_for_tests: Some(false),
        };
        assembly_tool.run(&args.to_vec()).unwrap();

        let _outputs = ProductAssemblyOutputs::from(args);

        let expected_commands: ToolCommandLog = serde_json::from_value(json!({
            "commands": [
                {
                    "tool": "./host_x64/assembly",
                    "args": [
                        "product",
                        "--product",
                        product,
                        "--board-config",
                        board_config,
                        "--outdir",
                        outdir,
                        "--input-bundles-dir",
                        input_bundles_dir,
                        "--gendir",
                        gendir,
                    ]
                }
            ]
        }))
        .unwrap();
        assert_eq!(&expected_commands, tools.log());
    }

    #[test]
    fn create_system() {
        let tools = FakeToolProvider::default();
        let assembly_tool = tools.get_tool("assembly").unwrap();

        let platform = Utf8PathBuf::from("path/to/platform");
        let iac = Utf8PathBuf::from("path/to/image_assembly_config");
        let outdir = Utf8PathBuf::from("path/to/outdir");
        let gendir = Utf8PathBuf::from("path/to/gendir");

        let args = CreateSystemArgs {
            platform: platform.clone(),
            image_assembly_config: iac.clone(),
            outdir: outdir.clone(),
            gendir: gendir.clone(),
            include_account: None,
            base_package_name: None,
        };
        assembly_tool.run(&args.to_vec()).unwrap();

        let _outputs = CreateSystemArgs::from(args);

        let expected_commands: ToolCommandLog = serde_json::from_value(json!({
            "commands": [
                {
                    "tool": "./host_x64/assembly",
                    "args": [
                        "create-system",
                        "--platform",
                        platform,
                        "--image-assembly-config",
                        iac,
                        "--outdir",
                        outdir,
                        "--gendir",
                        gendir,
                    ]
                }
            ]
        }))
        .unwrap();
        assert_eq!(&expected_commands, tools.log());
    }
}
