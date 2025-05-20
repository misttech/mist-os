// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::num::NonZeroU32;
use std::time::Duration;

use argh::{ArgsInfo, FromArgs};
use ffx_core::ffx_command;
use fidl_fuchsia_developer_ffx_speedtest as fspeedtest;

#[ffx_command()]
#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "speedtest",
    description = "Developer tool for testing fffx latency and throughput to the target."
)]
pub struct SpeedtestCommand {
    /// how many times to repeat the test. Set zero to run until interrupted.
    #[argh(option, short = 'r', default = "1")]
    pub repeat: usize,
    /// time to delay between repetitions, in milliseconds.
    #[argh(
        option,
        short = 'I',
        from_str_fn(duration_from_millis),
        default = "Duration::from_secs(1)"
    )]
    pub delay: Duration,
    #[argh(subcommand)]
    pub cmd: Subcommand,
}

fn duration_from_millis(value: &str) -> Result<Duration, String> {
    u64::from_str_radix(value, 10)
        .map(Duration::from_millis)
        .map_err(|_| format!("failed to parse milliseconds value from '{value}'"))
}

#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(subcommand)]
pub enum Subcommand {
    Ping(Ping),
    Socket(Socket),
}

/// Calculates latency to the target with simple channel messages.
#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "ping")]
pub struct Ping {
    /// the number of probes to send to calculate average latency.
    #[argh(option, short = 'c', default = "NonZeroU32::new(10).unwrap()")]
    pub count: NonZeroU32,
}

const DEFAULT_TRANSFER_MB: NonZeroU32 =
    NonZeroU32::new(fspeedtest::DEFAULT_TRANSFER_SIZE / 1_000_000).unwrap();
const DEFAULT_BUFFER_KB: NonZeroU32 =
    NonZeroU32::new(fspeedtest::DEFAULT_BUFFER_SIZE >> 10).unwrap();

/// Calculates throughput to the target using zircon socket abstractions.
#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "socket")]
pub struct Socket {
    /// transfer size in Mega Bytes (MB).
    #[argh(option, short = 'L', default = "DEFAULT_TRANSFER_MB")]
    pub transfer_mb: NonZeroU32,
    /// buffer size in Kilo Bytes (KiB).
    #[argh(option, short = 'b', default = "DEFAULT_BUFFER_KB")]
    pub buffer_kb: NonZeroU32,
    /// perform target->host transfer. host->target transfer is performed by
    /// default.
    #[argh(switch, short = 'R')]
    pub rx: bool,
}
