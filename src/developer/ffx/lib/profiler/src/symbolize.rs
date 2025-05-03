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
use std::sync::Arc;
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
    pub records: HashMap<Pid, Vec<SymbolizedRecord>>,
}

/// Symbolized bt list for a single tid.
#[derive(Clone, Debug)]
pub struct SymbolizedRecord {
    pub tid: Tid,
    backtraces: Vec<ResolvedAddress>,
}

impl SymbolizedRecord {
    fn add_backtrace(&mut self, backtrace: ResolvedAddress) {
        self.backtraces.push(backtrace);
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
        let context_arc = Arc::new(context.clone());
        let res: HashMap<Pid, Vec<SymbolizedRecord>> = self.handlers.par_iter().map(|(pid, handler):(&Pid, &ProfilingRecordHandler)| -> Result<(Pid, Vec<SymbolizedRecord>), SymbolizeError> {
            let context_clone_for_thread = Arc::clone(&context_arc);
            let mut res_per_pid = vec![];
                    let mut symbolizer = Symbolizer::with_context(&*context_clone_for_thread)?;
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
                        let mut symbolized_record =
                            SymbolizedRecord { tid: *tid, backtraces: Vec::new() };
                        for backtrace in backtraces {
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
                            symbolized_record.add_backtrace(resolved_addr);
                        }
                        res_per_pid.push(symbolized_record);
                    }
                    Ok((*pid, res_per_pid))
        })
        .collect::<Result<HashMap<Pid, Vec<SymbolizedRecord>>, SymbolizeError>>()?;
        if !pprof_conversion {
            std::fs::write(output, format!("{res:#?}\n"))?;
        }
        Ok(SymbolizedRecords { records: res })
    }
}
