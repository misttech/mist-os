// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::parse::{
    BacktraceDetails, ModuleWithMmapDetails, Pid, ProfilingRecordHandler, SymbolizeError, Tid,
    UnsymbolizedSamples,
};
use ffx_symbolize::{ResolvedLocation, Symbolizer};
use rayon::prelude::*;
use std::collections::HashMap;
use std::path::PathBuf;

// It defines how many processes a symbolizer will handle.
// We create a symbolizer for every thread.
// More threads => more symbolizers => more latency, but higher throughput.
// NUM_PROCESS_PER_THREAD is a hard coded number considering the trade off above.
static NUM_PROCESS_PER_THREAD: usize = 4;

/// A resolved address.
#[derive(Clone, PartialEq)]
pub struct ResolvedAddress {
    /// Address for which source locations were resolved.
    pub addr: u64,
    /// Source locations found at `addr`.
    pub locations: Vec<ResolvedLocation>,
}

impl std::fmt::Debug for ResolvedAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedAddress")
            .field("addr", &format_args!("0x{:x}", self.addr))
            .field("lines", &self.locations)
            .finish()
    }
}

/// Symbolized record hash map. key: pid, value: all of the records belong to this pid.
#[derive(Clone, Debug, Default)]
pub struct SymbolizedRecords {
    pub records: Vec<(Pid, Vec<SymbolizedRecord>)>,
}

/// Symbolized bt list for a single tid.
#[derive(Clone, Debug)]
pub struct SymbolizedRecord {
    pub tid: Tid,
    pub call_stacks: Vec<Vec<ResolvedAddress>>,
}

impl SymbolizedRecord {
    fn add_backtraces(&mut self, backtraces: Vec<ResolvedAddress>) {
        self.call_stacks.push(backtraces);
    }
}

pub fn symbolize(
    input: &PathBuf,
    output: &PathBuf,
    pprof_conversion: bool,
) -> Result<SymbolizedRecords, SymbolizeError> {
    let context =
        ffx_config::global_env_context().ok_or(SymbolizeError::NoFfxEnvironmentContext)?;
    symbolize_with_context(input, output, pprof_conversion, &context)
}

pub fn symbolize_with_context(
    input: &PathBuf,
    output: &PathBuf,
    pprof_conversion: bool,
    context: &ffx_config::EnvironmentContext,
) -> Result<SymbolizedRecords, SymbolizeError> {
    logging_rust_cpp_bridge::init_with_log_severity(logging_rust_cpp_bridge::FUCHSIA_LOG_FATAL);
    let unsymbolized_samples = UnsymbolizedSamples::new(input)?;
    unsymbolized_samples.process_unsymbolized_samples(output, pprof_conversion, &context)
}

impl UnsymbolizedSamples {
    pub fn process_unsymbolized_samples(
        self,
        output: &PathBuf,
        pprof_conversion: bool,
        context: &ffx_config::EnvironmentContext,
    ) -> Result<SymbolizedRecords, SymbolizeError> {
        let handlers: Vec<(Pid, ProfilingRecordHandler)> = self.handlers.into_iter().collect();
        let symbolized_samples = handlers.par_iter().chunks(NUM_PROCESS_PER_THREAD).map(|chunk| -> Result<Vec<(Pid, Vec<SymbolizedRecord>)>, SymbolizeError> {
        let mut symbolizer = Symbolizer::with_context(context)?;
        let symbolized_samples_per_thread: Result<Vec<(Pid, Vec<SymbolizedRecord>)>, SymbolizeError> = chunk.into_iter().map(|(pid, handler):&(Pid, ProfilingRecordHandler)| -> Result<(Pid, Vec<SymbolizedRecord>), SymbolizeError> {
                    let mut res_per_pid = vec![];
                    // We use a hashmap to store the seen backtrace, to avoid symbolize the same backtrace multiple times.
                    let mut seen_bt: HashMap<BacktraceDetails, ResolvedAddress> = HashMap::new();
                    for ModuleWithMmapDetails { module, mmaps } in
                        handler.get_module_with_mmap_records()
                    {
                        let module_id = symbolizer
                            .add_module(&module.name, hex::decode(&module.build_id)?.as_ref());

                        for mmap_record in mmaps {
                            symbolizer.add_mapping(module_id, mmap_record.clone())?;
                        }
                    }
                    for (tid, backtraces) in handler.get_backtrace_records() {
                        let mut symbolized_record = SymbolizedRecord {
                            tid: *tid,
                            call_stacks: Vec::new(),
                        };

                        for call_stack in backtraces {
                            let mut current_call_stack = vec![];
                            for backtrace in call_stack {
                                let resolved_addr =
                                    seen_bt.entry(*backtrace).or_insert_with_key(|bt_key| {
                                        let resolved_locations = symbolizer
                                            .resolve_addr(bt_key.0)
                                            .unwrap_or_else(|_| Vec::new());
                                        ResolvedAddress {
                                            addr: bt_key.0,
                                            locations: resolved_locations,
                                        }
                                    }).to_owned();
                                current_call_stack.push(resolved_addr);
                            }
                            symbolized_record.add_backtraces(current_call_stack);
                        }
                        res_per_pid.push(symbolized_record);
                    }
                    symbolizer.reset();
                    Ok((*pid, res_per_pid))
        })
        .collect::<Result<Vec<(Pid, Vec<SymbolizedRecord>)>, SymbolizeError>>();
        symbolized_samples_per_thread
        }).collect::<Result<Vec<Vec<(Pid, Vec<SymbolizedRecord>)>>, SymbolizeError>>()?;
        let symbolized_samples = symbolized_samples.into_iter().flatten().collect();
        if !pprof_conversion {
            std::fs::write(output, format!("{symbolized_samples:#?}\n"))?;
        }
        Ok(SymbolizedRecords { records: symbolized_samples })
    }
}
