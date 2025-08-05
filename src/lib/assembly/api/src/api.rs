// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use assembly_cli_args::{
    CreateSystemArgs, CreateSystemOutputs, ProductArgs, ProductAssemblyOutputs,
};
use assembly_tool::{PlatformToolProvider, ToolProvider};

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
    let product_outputs = product_assembly(args)?;
    create_system(product_outputs.into())
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
        let board_info = Utf8PathBuf::from("path/to/board_info");
        let outdir = Utf8PathBuf::from("path/to/outdir");
        let gendir = Utf8PathBuf::from("path/to/gendir");
        let input_bundles_dir = Utf8PathBuf::from("path/to/bundles");

        let args = ProductArgs {
            product: product.clone(),
            board_info: board_info.clone(),
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
                        "--board-info",
                        board_info,
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
