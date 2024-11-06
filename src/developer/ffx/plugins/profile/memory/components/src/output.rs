// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use prettytable::{row, Table};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fmt::Display;

use fidl_fuchsia_memory_attribution_plugin as fplugin;
use fplugin::Vmo;

use crate::{Principal, PrincipalIdentifier, Resource};

/// Consider that two floats are equals if they differ less than [FLOAT_COMPARISON_EPSILON].
const FLOAT_COMPARISON_EPSILON: f64 = 1e-10;

#[derive(Default)]
pub struct PluginOutput {
    pub principals: Vec<PrincipalOutput>,
    pub undigested: u64,
    pub kernel_stats: KernelStatistics,
}

impl Display for PluginOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "{}", self.kernel_stats)?;
        for principal in &self.principals {
            writeln!(f, "{}", principal.to_string())?;
        }
        Ok(())
    }
}

impl PluginOutput {
    pub fn build(
        principals: HashMap<PrincipalIdentifier, RefCell<Principal>>,
        resources: HashMap<u64, RefCell<Resource>>,
        kernel_stats: KernelStatistics,
    ) -> PluginOutput {
        let mut output = PluginOutput::default();
        for principal in principals.values() {
            output.principals.push(PluginOutput::build_one_principal(
                &principal,
                &principals,
                &resources,
            ));
        }

        output.principals.sort_unstable_by_key(|p| -(p.populated_total as i64));

        let mut undigested = 0;
        for (_, resource_ref) in &resources {
            let resource = &resource_ref.borrow();
            if resource.claims.is_empty() {
                match &resource.resource_type {
                    fplugin::ResourceType::Job(_) => println!("{:#?}", resource),
                    fplugin::ResourceType::Process(_) => println!("{:#?}", resource),
                    fplugin::ResourceType::Vmo(vmo) => {
                        undigested += vmo.committed_bytes.unwrap();
                    }
                    _ => todo!(),
                }
            }
        }
        output.undigested = undigested;
        output.kernel_stats = kernel_stats;
        output
    }

    fn build_one_principal(
        principal_cell: &RefCell<Principal>,
        principals: &HashMap<PrincipalIdentifier, RefCell<Principal>>,
        resources: &HashMap<u64, RefCell<Resource>>,
    ) -> PrincipalOutput {
        let principal = principal_cell.borrow();
        let mut output = PrincipalOutput {
            name: principal.name().to_owned(),
            id: principal.identifier.0,
            principal_type: match &principal.principal_type {
                fplugin::PrincipalType::Runnable => "R",
                fplugin::PrincipalType::Part => "P",
                fplugin::PrincipalTypeUnknown!() => unimplemented!(),
            }
            .to_owned(),
            committed_private: 0,
            committed_scaled: 0.0,
            committed_total: 0,
            populated_private: 0,
            populated_scaled: 0.0,
            populated_total: 0,
            attributor: principal
                .parent
                .as_ref()
                .map(|p| principals.get(p))
                .flatten()
                .map(|p| p.borrow().name().to_owned()),
            processes: Vec::new(),
            vmos: HashMap::new(),
        };

        for resource_id in &principal.resources {
            if !resources.contains_key(resource_id) {
                continue;
            }

            let resource = resources.get(resource_id).unwrap().borrow();
            let share_count = resource
                .claims
                .iter()
                .map(|c| c.subject)
                .collect::<HashSet<PrincipalIdentifier>>()
                .len();
            match &resource.resource_type {
                fplugin::ResourceType::Job(_) => todo!(),
                fplugin::ResourceType::Process(_) => {
                    output.processes.push(format!("{} ({})", resource.name.clone(), resource.koid));
                }
                fplugin::ResourceType::Vmo(vmo_info) => {
                    output.committed_total += vmo_info.committed_bytes.unwrap();
                    output.populated_total += vmo_info.populated_bytes.unwrap();
                    output.committed_scaled +=
                        vmo_info.committed_bytes.unwrap() as f64 / share_count as f64;
                    output.populated_scaled +=
                        vmo_info.populated_bytes.unwrap() as f64 / share_count as f64;
                    if share_count == 1 {
                        output.committed_private += vmo_info.committed_bytes.unwrap();
                        output.populated_private += vmo_info.populated_bytes.unwrap();
                    }
                    output
                        .vmos
                        .entry(
                            ffx_profile_memory_common::vmo_name_to_digest_name(&resource.name)
                                .to_owned(),
                        )
                        .or_default()
                        .merge(vmo_info, share_count);
                }
                _ => todo!(),
            }
        }

        for (_source, attribution) in &principal.attribution_claims {
            for resource in attribution.resources.as_ref().unwrap() {
                if let fplugin::ResourceReference::ProcessMapped(fplugin::ProcessMapped {
                    process: process_mapped,
                    base: _,
                    len: _,
                }) = resource
                {
                    if let Some(process_ref) = resources.get(&process_mapped) {
                        let process = process_ref.borrow();
                        output.processes.push(format!(
                            "{} ({})",
                            process.name.clone(),
                            process.koid
                        ));
                    }
                }
            }
        }

        output.processes.sort();
        output
    }
}

#[derive(Debug)]
pub struct PrincipalOutput {
    pub id: u64,
    pub name: String,
    pub principal_type: String,
    pub committed_private: u64,
    pub committed_scaled: f64,
    pub committed_total: u64,
    pub populated_private: u64,
    pub populated_scaled: f64,
    pub populated_total: u64,

    pub attributor: Option<String>,
    pub processes: Vec<String>,
    pub vmos: HashMap<String, VmoOutput>,
}

impl PrincipalOutput {
    fn to_string(&self) -> String {
        let mut s = String::new();
        if self.committed_private == self.populated_private
            && self.committed_scaled == self.populated_scaled
            && self.committed_total == self.populated_total
        {
            s += &format!(
                "{} ({}, {}): {} {} {}\n",
                self.name,
                self.id,
                self.principal_type,
                format_bytes(self.committed_private as f64),
                format_bytes(self.committed_scaled),
                format_bytes(self.committed_total as f64),
            );
        } else {
            s += &format!(
                "{} ({}, {}): {} ({}), {} ({}), {} ({})\n",
                self.name,
                self.id,
                self.principal_type,
                format_bytes(self.committed_private as f64),
                format_bytes(self.populated_private as f64),
                format_bytes(self.committed_scaled),
                format_bytes(self.populated_scaled),
                format_bytes(self.committed_total as f64),
                format_bytes(self.populated_total as f64)
            );
        }
        if let Some(parent) = &self.attributor {
            s += &format!("Attributor: {}\n", parent);
        }

        if !self.processes.is_empty() {
            s += &format!("Processes: {}\n", self.processes.join(", "));
        }

        let mut vmos: Vec<(&String, &VmoOutput)> = self.vmos.iter().collect();
        vmos.sort_by_key(|(_, v)| -(v.populated_total as i64));
        let mut table = Table::new();
        let format = prettytable::format::FormatBuilder::new().padding(1, 1).build();
        table.set_format(format);
        table.add_row(
            row![bc -> "VMO name", bc->"Count", bH2c->"Private", bH2c->"Scaled", bH2c->"Total"],
        );
        table.add_row(row![bH2 -> "", bc->"Committed", bc->"Populated", bc->"Committed", bc->"Populated", bc->"Committed", bc->"Populated"]);
        for (name, vmo) in vmos {
            table.add_row(row![
                name,
                vmo.count,
                format_bytes(vmo.committed_private as f64),
                format_bytes(vmo.populated_private as f64),
                format_bytes(vmo.committed_scaled),
                format_bytes(vmo.populated_scaled),
                format_bytes(vmo.committed_total as f64),
                format_bytes(vmo.populated_total as f64)
            ]);
        }
        s += &table.to_string();
        s
    }
}

impl PartialEq for PrincipalOutput {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
            && self.name == other.name
            && self.principal_type == other.principal_type
            && self.committed_private == other.committed_private
            && (self.committed_scaled - other.committed_scaled).abs() < FLOAT_COMPARISON_EPSILON
            && self.committed_total == other.committed_total
            && self.populated_private == other.populated_private
            && (self.populated_scaled - other.populated_scaled).abs() < FLOAT_COMPARISON_EPSILON
            && self.populated_total == other.populated_total
            && self.attributor == other.attributor
            && self.processes == other.processes
            && self.vmos == other.vmos
    }
}

/// Group of VMOs sharing the same name.
#[derive(Default, Debug)]
pub struct VmoOutput {
    /// Number of distinct VMOs under the same name.
    pub count: u64,
    /// Aggregated statistics of this VMO group
    pub committed_private: u64,
    pub committed_scaled: f64,
    pub committed_total: u64,
    pub populated_private: u64,
    pub populated_scaled: f64,
    pub populated_total: u64,
}

impl VmoOutput {
    fn merge(&mut self, vmo_info: &Vmo, share_count: usize) {
        self.count += 1;
        self.committed_total += vmo_info.committed_bytes.unwrap();
        self.populated_total += vmo_info.populated_bytes.unwrap();
        self.committed_scaled += vmo_info.committed_bytes.unwrap() as f64 / share_count as f64;
        self.populated_scaled += vmo_info.populated_bytes.unwrap() as f64 / share_count as f64;
        if share_count == 1 {
            self.committed_private += vmo_info.committed_bytes.unwrap();
            self.populated_private += vmo_info.populated_bytes.unwrap();
        }
    }
}

impl PartialEq for VmoOutput {
    fn eq(&self, other: &Self) -> bool {
        self.count == other.count
            && self.committed_private == other.committed_private
            && (self.committed_scaled - other.committed_scaled).abs() < FLOAT_COMPARISON_EPSILON
            && self.committed_total == other.committed_total
            && self.populated_private == other.populated_private
            && (self.populated_scaled - other.populated_scaled).abs() < FLOAT_COMPARISON_EPSILON
            && self.populated_total == other.populated_total
    }
}

#[derive(Default)]
pub struct KernelStatistics {
    /// Total physical memory available to the system, in bytes.
    pub total: u64,
    /// Unallocated memory, in bytes.
    pub free: u64,
    pub kernel_total: u64,
    /// Memory reserved by and mapped into the kernel for reasons
    /// not covered by other fields in this struct, in
    /// bytes. Typically for readonly data like the ram disk and
    /// kernel image, and for early-boot dynamic memory.
    pub wired: u64,
    /// Memory allocated to the kernel heap, in bytes.
    pub total_heap: u64,
    /// Memory committed to (reserved for, but not necessarily
    /// used by) VMOs, both kernel and user, in bytes. A superset
    /// of all userspace memory. Does not include certain VMOs
    /// that fall under `wired`.
    pub vmo: u64,
    /// Memory used for architecture-specific MMU (Memory
    /// Management Unit) metadata like page tables, in bytes.
    pub mmu: u64,
    /// Memory in use by IPC, in bytes.
    pub ipc: u64,
    /// Non-free memory that isn't accounted for in any other
    /// field, in bytes.
    pub other: u64,
    /// On a system with zRAM, the size in bytes of all memory,
    /// including metadata, fragmentation and other overheads, of
    /// the compressed memory area.
    pub zram_compressed_total: Option<u64>,
}

impl Display for KernelStatistics {
    fn fmt(&self, w: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(w, "Total memory: {}", format_bytes(self.total as f64))?;
        writeln!(w, "Free memory: {}", format_bytes(self.free as f64))?;
        writeln!(w, "Kernel:    {}", format_bytes(self.kernel_total as f64))?;
        writeln!(w, "    wired: {}", format_bytes(self.wired as f64))?;
        writeln!(w, "    vmo:   {}", format_bytes(self.vmo as f64))?;
        writeln!(w, "    heap:  {}", format_bytes(self.total_heap as f64))?;
        writeln!(w, "    mmu:   {}", format_bytes(self.mmu as f64))?;
        writeln!(w, "    ipc:   {}", format_bytes(self.ipc as f64))?;
        if let Some(zram) = self.zram_compressed_total {
            writeln!(w, "    zram:  {}", format_bytes(zram as f64))?;
        }
        writeln!(w, "    other: {}", format_bytes(self.other as f64))?;
        writeln!(w)
    }
}

pub fn format_bytes(bytes: f64) -> String {
    if bytes < 1024.0 {
        format!("{:0.2} B", bytes)
    } else if bytes / 1024.0 < 1024.0 {
        format!("{:0.2} KiB", bytes / 1024.0)
    } else {
        format!("{:0.2} MiB", bytes / (1024.0 * 1024.0))
    }
}
