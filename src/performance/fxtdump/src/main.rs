// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use argh::{ArgsInfo, FromArgs};
use fxt::session::SessionParser;
use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;

#[derive(FromArgs)]
/// Print information about an FXT file.
struct Args {
    #[argh(subcommand)]
    command: FxtDump,
}

#[derive(ArgsInfo, FromArgs, PartialEq, Debug)]
#[argh(subcommand)]
pub enum FxtDump {
    Dump(Dump),
    CategoryStats(CategoryStats),
}

#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "dump",
    description = "Dump a human readable view of the trace",
    example = "fx fxtdump dump trace.fxt"
)]
pub struct Dump {
    #[argh(positional)]
    pub input: String,
    /// if set, immediately abort when encountering an invalid trace event.
    #[argh(switch)]
    /// if set, immediately abort when encountering an invalid trace event.
    pub strict: bool,
}

#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "category_stats",
    description = "Print information about the count and total size of each category",
    example = "fx fxtdump category_stats trace.fxt"
)]
pub struct CategoryStats {
    #[argh(positional)]
    pub input: String,
}

fn dump<R: std::io::Read>(fxt: &mut SessionParser<R>, strict: bool) {
    let mut offset = 0;
    while let Some(record) = fxt.next() {
        match record {
            Ok(trace_record) => {
                println!("{:#010x}: {:?}", offset, trace_record);
            }
            Err(parse_err) => {
                if strict {
                    eprintln!("ERROR {:#}", parse_err);
                    return;
                } else {
                    eprintln!("WARNING {:#}", parse_err);
                }
            }
        }
        offset += fxt.parsed_bytes().len();
    }
}

fn category_stats<R: std::io::Read>(fxt: &mut SessionParser<R>) {
    let mut category_counts = HashMap::<String, usize>::new();
    let mut category_sizes = HashMap::<String, usize>::new();

    while let Some(record) = fxt.next() {
        match record {
            Ok(trace_record) => match trace_record {
                fxt::TraceRecord::Event(event_record) => {
                    *category_counts.entry(event_record.category.as_str().into()).or_insert(0) += 1;
                    *category_sizes.entry(event_record.category.as_str().into()).or_insert(0) +=
                        fxt.parsed_bytes().len();
                }
                fxt::TraceRecord::Scheduling(_scheduling_record) => {
                    *category_counts.entry("kernel:sched".into()).or_insert(0) += 1;
                    *category_sizes.entry("kernel:sched".into()).or_insert(0) +=
                        fxt.parsed_bytes().len();
                }
                fxt::TraceRecord::Blob(blob_record) => {
                    *category_counts.entry(blob_record.name.as_str().into()).or_insert(0) += 1;
                    *category_sizes.entry(blob_record.name.as_str().into()).or_insert(0) +=
                        fxt.parsed_bytes().len();
                }
                fxt::TraceRecord::LargeBlob(large_blob_record) => {
                    *category_counts
                        .entry(large_blob_record.category.as_str().into())
                        .or_insert(0) += 1;
                    *category_sizes
                        .entry(large_blob_record.category.as_str().into())
                        .or_insert(0) += fxt.parsed_bytes().len();
                }
                fxt::TraceRecord::KernelObj(_kernel_obj_record) => {
                    *category_counts.entry("kernel:meta".into()).or_insert(0) += 1;
                    *category_sizes.entry("kernel:meta".into()).or_insert(0) +=
                        fxt.parsed_bytes().len();
                }
                fxt::TraceRecord::UserspaceObj(_user_obj_record) => {
                    *category_counts.entry("user:meta".into()).or_insert(0) += 1;
                    *category_sizes.entry("user:meta".into()).or_insert(0) +=
                        fxt.parsed_bytes().len();
                }
                fxt::TraceRecord::Log(_log_record) => {
                    *category_counts.entry("log".into()).or_insert(0) += 1;
                    *category_sizes.entry("log".into()).or_insert(0) += fxt.parsed_bytes().len();
                }
                fxt::TraceRecord::ProviderEvent { .. } => {
                    *category_counts.entry("metadata".into()).or_insert(0) += 1;
                    *category_sizes.entry("metadata".into()).or_insert(0) +=
                        fxt.parsed_bytes().len();
                }
            },
            Err(parse_err) => eprintln!("WARNING {:#}", parse_err),
        }
    }

    let mut sorted_counts: Vec<_> = category_counts.into_iter().collect();
    sorted_counts.sort_by(|a, b| b.1.cmp(&a.1));

    println!("{:26}{:16}    {:10}        ", "Category", "Size", "Count");
    println!();
    for (category, count) in sorted_counts {
        println!(
            "{:20}  {:10} KiB  {:10} records",
            category,
            category_sizes[&*category] / 1024,
            count
        );
    }
}

fn main() -> Result<()> {
    let args: Args = argh::from_env();

    match args.command {
        FxtDump::CategoryStats(CategoryStats { input }) => {
            let file = File::open(input)?;
            let mut reader = BufReader::new(file);
            let mut session = SessionParser::new(&mut reader);
            category_stats(&mut session)
        }
        FxtDump::Dump(Dump { input, strict }) => {
            let file = File::open(input)?;
            let mut reader = BufReader::new(file);
            let mut session = SessionParser::new(&mut reader);
            dump(&mut session, strict)
        }
    }

    Ok(())
}
