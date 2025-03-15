// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::FromArgs;
use std::fs;
use std::path::Path;

#[derive(FromArgs, Debug)]
/// Replaces the library name in a FIDL file.
struct Args {
    #[argh(option, description = "original library name")]
    original_library: String,

    #[argh(option, description = "new library name")]
    new_library: String,

    #[argh(option, description = "path to the input FIDL file")]
    input_file: String,

    #[argh(option, description = "path to the output FIDL file")]
    output_file: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Args = argh::from_env();

    if !Path::new(&args.input_file).exists() {
        return Err(format!("Error: File not found at '{}'", args.input_file).into());
    }

    let input_fidl = fs::read_to_string(&args.input_file)?;
    let original_library_pattern = format!("library {};", args.original_library);
    let new_library_pattern = format!("library {};", args.new_library);

    let first_occurrence = input_fidl.find(&original_library_pattern);
    let last_occurrence = input_fidl.rfind(&original_library_pattern);

    if first_occurrence.is_none() {
        return Err(format!("Error: File does not contain '{original_library_pattern}'").into());
    }
    if first_occurrence != last_occurrence {
        return Err(format!(
            "Error: File contains multiple occurrences of '{original_library_pattern}'"
        )
        .into());
    }

    let updated_fidl = input_fidl.replace(&original_library_pattern, &new_library_pattern);

    fs::write(&args.output_file, updated_fidl)?;

    Ok(())
}
