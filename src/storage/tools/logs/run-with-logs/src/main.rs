// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    argh::FromArgs,
    byteorder::{LittleEndian, WriteBytesExt},
    fidl_fuchsia_hardware_block::BlockMarker,
    fuchsia_component::client::connect_channel_to_protocol_at_path,
    remote_block_device::{Cache, RemoteBlockClientSync},
    std::{
        io::Write,
        process::{Command, Output},
    },
};

/// Magic number we write to the disk before the log data. This allows the extractor to
/// differentiate between failures of this harness and a successful run where no output was
/// received, which would otherwise both be all zeros.
///
/// It says logs.
const MAGIC: u64 = 0x1092;

/// Run a binary, writing stdout and stderr to a block device so it can be extracted from the device.
#[derive(FromArgs)]
struct Args {
    /// block device to write the test output to.
    #[argh(positional)]
    block_device: String,
    /// binary to run and capture the output of.
    #[argh(positional)]
    binary: String,
    /// any additional arguments for the binary.
    #[argh(positional, greedy)]
    args: Vec<String>,
}

fn main() {
    let Args { block_device, binary, args } = argh::from_env();

    // Run the test process, extracting stdout and stderr. The `output` function reads everything off
    // of stdout and stderr before returning, so we don't need to worry about the process exiting
    // too fast. `output` will return Ok even if the exit status is non-zero, it only fails if the
    // command fails to execute in the first place, like if the binary doesn't exist. We capture
    // those errors and attempt to write them to stderr in place of the command output.
    let (stdout, stderr) = match Command::new(binary).args(&args).output() {
        Ok(Output { stdout, stderr, status: _ }) => (stdout, stderr),
        Err(e) => (Vec::new(), format!("command failed: {:?}", e).into_bytes()),
    };

    // Write the test process output to the block device in the expected format.
    let (client_end, server_end) = fidl::endpoints::create_endpoints::<BlockMarker>();
    connect_channel_to_protocol_at_path(server_end.into_channel(), &block_device)
        .expect("connecting to block device");

    let block_client = RemoteBlockClientSync::new(client_end).expect("making remote block client");
    let mut cache = Cache::new(block_client).expect("making block client cache");

    // Format is <magic><length><data><length><data> where length is a u64, first for stdout then
    // stderr. The magic differentiates between an empty disk and one that has been written to.
    cache.write_u64::<LittleEndian>(MAGIC).expect("writing magic");
    cache
        .write_u64::<LittleEndian>(stdout.len().try_into().expect("usize to u64 conversion"))
        .expect("writing stdout length");
    cache.write_all(&stdout).expect("writing stdout");
    cache
        .write_u64::<LittleEndian>(stderr.len().try_into().expect("usize to u64 conversion"))
        .expect("writing stderr length");
    cache.write_all(&stderr).expect("writing stderr");
}
