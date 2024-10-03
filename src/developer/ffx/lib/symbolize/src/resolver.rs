// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::parse::{ParseError, ResolvedAddress};
use crate::symbolizer::{CreateSymbolizerError, Symbolizer, SymbolizerOptions};
use bitflags::bitflags;
use ffx_config::EnvironmentContext;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;

/// Resolves addresses to symbolic names and source locations.
#[derive(Debug)]
pub struct Resolver {
    next_id: u64,
    modules: BTreeMap<ModuleId, Module>,
    symbolizer: Symbolizer,
    extra_symbol_dirs: Vec<PathBuf>,
}

impl Resolver {
    /// Create a new resolver. `async` to support interacting with ffx machinery.
    pub async fn new() -> Result<Self, ResolverError> {
        Ok(Self {
            next_id: 0,
            modules: Default::default(),
            extra_symbol_dirs: vec![],
            symbolizer: Symbolizer::new().await?,
        })
    }

    /// Create a new resolver with a specific ffx context. Normally only needed in tests.
    pub async fn with_context(context: &EnvironmentContext) -> Result<Self, ResolverError> {
        Ok(Self {
            next_id: 0,
            modules: Default::default(),
            extra_symbol_dirs: vec![],
            symbolizer: Symbolizer::with_context(context).await?,
        })
    }

    /// Add a new module from the process, returning a unique ID that can be used to associate
    /// multiple mappings with the same module.
    pub fn add_module(&mut self, name: &str, build_id: &[u8]) -> ModuleId {
        let id = ModuleId(self.next_id);
        self.next_id += 1;
        self.modules.insert(
            id,
            Module { name: name.to_string(), build_id: build_id.to_vec(), mappings: vec![] },
        );
        id
    }

    /// Add a new mapping for a given module in the process.
    pub fn add_mapping(&mut self, id: ModuleId, details: MappingDetails) {
        let module = self.modules.get_mut(&id).expect("ModuleId's are never removed");
        module.mappings.push(details);
    }

    /// Add an additional directory of symbols data for resolution. Used primarily for testing.
    pub fn add_symbol_dir(&mut self, dir: impl AsRef<Path>) {
        self.extra_symbol_dirs.push(dir.as_ref().to_owned());
    }

    /// Resolve a single address.
    ///
    /// If you have more than one address to resolve, consider using `resolve_addresses` to batch
    /// them together since each call spawns a subprocess.
    pub fn resolve_addr(&self, addr: u64) -> Result<ResolvedAddress, ResolverError> {
        let mut symbols = self.resolve_addresses(&[addr])?;
        // If successful the number of resolved addresses matches the number of inputs.
        Ok(symbols.remove(0))
    }

    /// Resolve multiple addresses.
    pub fn resolve_addresses(
        &self,
        addresses: &[u64],
    ) -> Result<Vec<ResolvedAddress>, ResolverError> {
        // TODO(https://fxbug.dev/371055380) use debuginfod instead of symbolizer text output
        let symbolizer_outputs = self.run_symbolizer_for_addresses(addresses)?;
        let mut resolved = vec![];
        for address_output in symbolizer_outputs {
            resolved.push(ResolvedAddress::parse(&address_output).map_err(|error| {
                ResolverError::Parsing { full_symbolizer_output: address_output.clone(), error }
            })?);
        }
        Ok(resolved)
    }

    fn run_symbolizer_for_addresses(
        &self,
        addresses: &[u64],
    ) -> Result<Vec<String>, ResolverError> {
        // The symbolizer preserves non-backtrace lines that are interleaved with a backtrace, this
        // allows us to know for sure that a given set of lines correspond to a particular address
        // without needing to further parse frame numbers. This will go away once
        // https://fxbug.dev/371055380 is resolved.
        const ADDRESS_DELIMITER: &str = "<^><^><^><^><^><^><^><^><^><^><^>";
        let mut symbolizer_input = String::new();
        for (id, module) in &self.modules {
            symbolizer_input += &module.symbolizer_markup(*id);
        }

        for (i, addr) in addresses.iter().enumerate() {
            symbolizer_input += &symbolizer_markup_for_address(i, *addr);
            symbolizer_input += ADDRESS_DELIMITER;
            symbolizer_input.push('\n');
        }

        let mut options = SymbolizerOptions::default().omit_module_lines(true);
        for dir in &self.extra_symbol_dirs {
            options = options.symbol_path(dir);
        }

        let mut child = self
            .symbolizer
            .command(options)?
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(ResolverError::SpawnSymbolizer)?;

        let mut child_stdin = child.stdin.take().expect("passed piped stdin");
        child_stdin
            .write_all(symbolizer_input.as_bytes())
            .map_err(ResolverError::WriteToSymbolizerPipe)?;
        // Close stdin to finish and avoid indefinite blocking
        drop(child_stdin);

        let output = child.wait_with_output().map_err(ResolverError::WaitForSymbolizerOutput)?;
        eprintln!("symbolizer stderr: {}", String::from_utf8_lossy(&output.stderr));
        if !output.status.success() {
            return Err(ResolverError::SymbolizerFailed(output.status));
        }

        let symbolizer_output = String::from_utf8_lossy(&output.stdout);
        let raw_locations = symbolizer_output
            .split(ADDRESS_DELIMITER)
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect::<Vec<_>>();

        if raw_locations.len() != addresses.len() {
            return Err(ResolverError::MismatchedInputOutputCount {
                expected: addresses.len(),
                observed: raw_locations.len(),
            });
        }

        Ok(raw_locations)
    }
}

fn symbolizer_markup_for_address(frame_num: usize, addr: u64) -> String {
    format!("{{{{{{bt:{}:{:#018x}}}}}}}\n", frame_num, addr)
}

struct Module {
    name: String,
    build_id: Vec<u8>,
    mappings: Vec<MappingDetails>,
}

impl Module {
    fn symbolizer_markup(&self, id: ModuleId) -> String {
        let mut result = String::new();
        result += &format!(
            "{{{{{{module:{}:{}:elf:{}}}}}}}\n",
            id.0,
            self.name,
            hex::encode(&self.build_id)
        );

        for mapping in &self.mappings {
            result += &format!(
                "{{{{{{mmap:{:#02x}:{:#02x}:load:{}:{}:{:#02x}}}}}}}\n",
                mapping.start_addr, mapping.size, id.0, mapping.flags, mapping.vaddr,
            );
        }

        result
    }
}

impl std::fmt::Debug for Module {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Module")
            .field("name", &self.name)
            .field("build_id", &format_args!("0x{}", hex::encode(&self.build_id)))
            .field("mappings", &self.mappings)
            .finish()
    }
}

/// The ID of a module in the resolver, used to uniquely identify its mappings when symbolizing.
#[derive(Clone, Copy, Debug, Eq, PartialEq, PartialOrd, Ord)]
pub struct ModuleId(u64);

/// Details of a particular module's mapping. Each module is often mapped multiple times, once for
/// each PT_LOAD segment.
pub struct MappingDetails {
    /// The start of the mapping in the process' address space.
    pub start_addr: u64,
    /// The size of the mapping in bytes.
    pub size: u64,
    /// The p_vaddr value of the mapping, usually the offset of the mapping within the source file.
    pub vaddr: u64,
    /// Readable/writeable/executable flags.
    pub flags: MappingFlags,
}

impl std::fmt::Debug for MappingDetails {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MappingDetails")
            .field("start_addr", &format_args!("0x{:x}", self.start_addr))
            .field("size", &format_args!("0x{:x}", self.size))
            .field("vaddr", &format_args!("0x{:x}", self.vaddr))
            .field("flags", &self.flags)
            .finish()
    }
}

bitflags! {
    /// Flags for a module's mapping.
    #[derive(Debug)]
    pub struct MappingFlags: u32 {
        /// The mapping is readable.
        const READ = 0b1;
        /// The mapping is writeable.
        const WRITE = 0b10;
        /// The mapping is executable.
        const EXECUTE = 0b100;
    }
}

impl std::fmt::Display for MappingFlags {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.contains(Self::READ) {
            write!(f, "r")?;
        }
        if self.contains(Self::WRITE) {
            write!(f, "w")?;
        }
        if self.contains(Self::EXECUTE) {
            write!(f, "x")?;
        }
        Ok(())
    }
}

/// Errors that can occur when running the symbolizer.
#[derive(Debug, thiserror::Error)]
pub enum ResolverError {
    /// Failed to create symbolizer.
    #[error("Couldn't create symbolizer instance.")]
    CreateSymbolizer(
        #[from]
        #[source]
        CreateSymbolizerError,
    ),

    /// Failed to spawn symbolizer.
    #[error("Couldn't spawn symbolizer process.")]
    SpawnSymbolizer(#[source] std::io::Error),

    /// Failed to write symbolizer input.
    #[error("Couldn't write to symbolizer stdin.")]
    WriteToSymbolizerPipe(#[source] std::io::Error),

    /// Failed to wait for process exit and collect stdout.
    #[error("Failed collecting symbolizer output waiting for exit.")]
    WaitForSymbolizerOutput(#[source] std::io::Error),

    /// Symbolizer exited with non-zero exit code.
    #[error("Symbolizer failed to run, status={_0:?}")]
    SymbolizerFailed(std::process::ExitStatus),

    /// Got the wrong number of addresses from symbolizer output.
    #[error("Sent {expected} addresses to symbolizer, got {observed} back.")]
    MismatchedInputOutputCount {
        /// Number of addresses provided to symbolizer.
        expected: usize,
        /// Number of address outputs observed from symbolizer.
        observed: usize,
    },

    /// Couldn't parse output provided by the symbolizer.
    #[error("Failed to parse symbolizer output from `{full_symbolizer_output}`.")]
    Parsing {
        /// The full output from the symbolizer that was being resolved for an address.
        full_symbolizer_output: String,
        /// The underlying parse error encountered.
        #[source]
        error: ParseError,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_markup() {
        let module = Module {
            name: "libstd-0a3b4716ae52ea12.so".to_string(),
            build_id: 0x0c64a315fbd7579au64.to_ne_bytes().to_vec(),
            mappings: vec![
                MappingDetails {
                    start_addr: 0x1e874b65000,
                    size: 0x6f000,
                    vaddr: 0x0,
                    flags: MappingFlags::READ,
                },
                MappingDetails {
                    start_addr: 0x1e874bd4000,
                    size: 0xa1000,
                    vaddr: 0x6f000,
                    flags: MappingFlags::READ | MappingFlags::EXECUTE,
                },
                MappingDetails {
                    start_addr: 0x1e874c75000,
                    size: 0x8000,
                    vaddr: 0x110000,
                    flags: MappingFlags::READ | MappingFlags::WRITE,
                },
                MappingDetails {
                    start_addr: 0x1e874c7d000,
                    size: 0x1000,
                    vaddr: 0x118000,
                    flags: MappingFlags::READ | MappingFlags::WRITE,
                },
            ],
        };
        assert_eq!(
            module.symbolizer_markup(ModuleId(0)),
            "{{{module:0:libstd-0a3b4716ae52ea12.so:elf:9a57d7fb15a3640c}}}
{{{mmap:0x1e874b65000:0x6f000:load:0:r:0x0}}}
{{{mmap:0x1e874bd4000:0xa1000:load:0:rx:0x6f000}}}
{{{mmap:0x1e874c75000:0x8000:load:0:rw:0x110000}}}
{{{mmap:0x1e874c7d000:0x1000:load:0:rw:0x118000}}}
"
        );
    }

    #[test]
    fn address_markup() {
        assert_eq!(
            symbolizer_markup_for_address(0, 1910876255329),
            "{{{bt:0:0x000001bce919b461}}}\n"
        );
    }
}
