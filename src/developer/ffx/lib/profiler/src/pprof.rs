// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ffx_symbolize::ResolvedLocation;
pub use profile_rust_proto::perfetto::third_party::perftools::profiles as pproto;
use profile_rust_proto::perfetto::third_party::perftools::profiles::{
    Function, Label, Line, Location, ValueType,
};

use crate::symbolize::{ResolvedAddress, SymbolizedRecords};
use anyhow::Result;
use prost::Message;
use std::collections::HashMap;
use std::default::Default;
use std::path::PathBuf;

static PID_INTERN: i64 = 1;
static TID_INTERN: i64 = 2;

#[derive(Eq, Hash, PartialEq, Debug)]
struct FunctionEntry {
    name: i64,
    filename: i64,
    start_line: i64,
}

#[derive(Eq, Hash, PartialEq, Debug)]
struct LocationEntry {
    addr: u64,
    functions: Vec<u64>, // a list of function id.
}

/// Pprof interns most strings: it keeps a table of known strings and replaces
/// most occurrences of strings in its data by their index. This structure
/// implements that behavior.
#[derive(Default)]
struct StringTable {
    strings: HashMap<String, i64>,
    table: Vec<String>,
}

impl StringTable {
    pub fn new() -> StringTable {
        let mut st = StringTable { strings: HashMap::new(), table: vec![] };
        st.intern("");
        st.intern("pid");
        st.intern("tid");
        st
    }
    /// If a string is already known to the table, return its index. Otherwise,
    /// insert it and return its newly created index.
    pub fn intern(&mut self, string: &str) -> i64 {
        *self.strings.entry(string.to_string()).or_insert_with(|| {
            let index = self.table.len();
            self.table.push(string.to_string());
            index as i64
        })
    }
    fn finalize(self) -> Vec<String> {
        self.table
    }
}

#[derive(Default)]
pub struct ProfileBuilder {
    profile: pproto::Profile,
    st: StringTable,
    function_to_index: HashMap<FunctionEntry, u64>,
    location_to_index: HashMap<LocationEntry, u64>,
}

impl ProfileBuilder {
    pub fn new() -> ProfileBuilder {
        ProfileBuilder {
            profile: pproto::Profile::default(),
            st: StringTable::new(),
            function_to_index: HashMap::new(),
            location_to_index: HashMap::new(),
        }
    }

    pub fn build_profile(mut self, input: SymbolizedRecords) -> pproto::Profile {
        for (pid, record_per_thread) in input.records {
            let pid_label = Label {
                key: PID_INTERN,
                str: self.st.intern(pid.to_string().as_str()),
                num: pid.0 as i64,
                ..Default::default()
            };
            let pid_addr = ResolvedAddress {
                addr: 0xDEADBEEF,
                locations: vec![ResolvedLocation {
                    function: "pid: ".to_owned() + &pid.to_string(),
                    file_and_line: None,
                    library: None,
                    library_offset: 0,
                }],
            };
            for record in record_per_thread {
                for call_stack in record.call_stacks {
                    let mut sample = pproto::Sample::default();
                    let tid_label = Label {
                        key: TID_INTERN,
                        str: self.st.intern(record.tid.to_string().as_str()),
                        num: record.tid.0 as i64,
                        ..Default::default()
                    };
                    sample.label.push(pid_label.clone());
                    sample.label.push(tid_label);
                    let tid_addr = ResolvedAddress {
                        addr: 0xDEADBEEF,
                        locations: vec![ResolvedLocation {
                            function: "tid: ".to_owned() + &record.tid.to_string(),
                            file_and_line: None,
                            library: None,
                            library_offset: 0,
                        }],
                    };
                    for backtrace in call_stack {
                        sample.location_id.push(self.build_location(backtrace));
                    }
                    sample.location_id.push(self.build_location(tid_addr));
                    sample.location_id.push(self.build_location(pid_addr.clone()));

                    sample.value.push(1);
                    self.profile.sample.push(sample);
                }
            }
        }
        self.profile.sample_type =
            vec![ValueType { r#type: self.st.intern("samples"), unit: self.st.intern("count") }];
        self.profile.string_table = self.st.finalize();
        self.profile
    }

    // Add location to profile struct, and return the related location id.
    pub fn build_location(&mut self, address: ResolvedAddress) -> u64 {
        let mut location = Location::default();
        location.address = address.addr;
        let mut function_id_list = vec![];
        for function in address.locations {
            let (function_id, line) = self.build_function(function);
            let line = Line { function_id, line };
            function_id_list.push(function_id);
            location.line.push(line);
        }
        let location_entry = LocationEntry { addr: address.addr, functions: function_id_list };

        let location_id = self.location_to_index.entry(location_entry).or_insert_with(|| {
            let next_id = self.profile.location.len() as u64 + 1;
            location.id = next_id;
            self.profile.location.push(location);

            next_id
        });

        *location_id
    }

    // Add function to the profile struct, and return the related function id and line number.
    pub fn build_function(&mut self, address: ResolvedLocation) -> (u64, i64) {
        let mut function = Function::default();
        function.name = self.st.intern(&address.function);
        let mut line_number = 0;
        if let Some((file, line)) = address.file_and_line {
            function.filename = self.st.intern(&file);
            function.start_line = line as i64;
            line_number = function.start_line;
        }
        let function_entry = FunctionEntry {
            name: function.name,
            filename: function.filename,
            start_line: function.start_line,
        };

        let function_id = self.function_to_index.entry(function_entry).or_insert_with(|| {
            let next_id = self.profile.function.len() as u64 + 1;
            function.id = next_id;
            self.profile.function.push(function);

            next_id
        });

        (*function_id, line_number)
    }
}

pub fn samples_to_pprof(input: SymbolizedRecords, output: PathBuf) -> Result<()> {
    let builder = ProfileBuilder::new();
    let profile = builder.build_profile(input);
    std::fs::write(output, profile.encode_to_vec()).map_err(|e| anyhow::anyhow!(e))?;
    Ok(())
}
