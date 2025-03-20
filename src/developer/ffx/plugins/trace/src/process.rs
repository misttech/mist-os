// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Result};
use errors::ffx_bail;
use ffx_config::EnvironmentContext;
use ffx_trace::SymbolizationMap;
use fxt::{
    Arg, ArgValue, RawArg, RawArgValue, RawEventRecord, SessionParser, StringRef, TraceRecord,
};
use std::collections::HashMap;
use std::path::Path;
use termion::{color, style};

static ORDINAL_ARG_NAME: &str = "ordinal";

/// An iterator over trace files that yields each record in both parsed and raw form.
struct TraceIterator<R> {
    parser: SessionParser<R>,
}

impl<R: std::io::Read> TraceIterator<R> {
    fn new(reader: R) -> Self {
        Self { parser: SessionParser::new(reader) }
    }
}

impl TraceIterator<std::io::Cursor<Vec<u8>>> {
    fn from_bytes(bytes: Vec<u8>) -> Self {
        TraceIterator::new(std::io::Cursor::new(bytes))
    }
}

impl<R: std::io::Read> Iterator for TraceIterator<R> {
    type Item = Result<(TraceRecord, Vec<u8>)>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(record) = self.parser.next() {
            match record {
                Ok(record) => {
                    let parsed_bytes = self.parser.parsed_bytes().to_owned();
                    Some(Ok((record, parsed_bytes)))
                }
                Err(e) => Some(Err(anyhow!(e))),
            }
        } else {
            None
        }
    }
}

/// Parse a trace session and call a function for each trace record that is an event record.
/// The function takes the parsed event record and the raw bytes and returns raw bytes.
/// This allows the function to examine the event record and modify it if needed.
fn process_event_records<F: FnMut(fxt::EventRecord, Vec<u8>) -> Vec<u8>>(
    input: Vec<u8>,
    mut process: F,
) -> Result<Vec<u8>> {
    let output: Vec<u8> = TraceIterator::from_bytes(input)
        .flat_map(|r| match r {
            Ok((record, parsed_bytes)) => {
                if let TraceRecord::Event(event_record) = record {
                    process(event_record, parsed_bytes)
                } else {
                    parsed_bytes
                }
            }
            Err(err) => panic!("failed to process trace record: {err:?}"),
        })
        .collect();
    Ok(output)
}

/// Process a trace file.
/// Optionally symbolize and apply a category check.
pub fn process_trace_file(
    trace_file: impl AsRef<Path>,
    outfile: impl AsRef<Path>,
    symbolize: bool,
    category_check: Option<Vec<String>>,
    context: &EnvironmentContext,
) -> Result<Vec<String>> {
    let mut warning_list = Vec::new();

    let symbolizer = if symbolize { Some(Symbolizer::new(&context)) } else { None };
    let mut category_counter = if let Some(input_categories) = category_check {
        Some(CategoryCounter::new(input_categories))
    } else {
        None
    };

    // Read the trace into memory.
    let trace = std::fs::read(&trace_file)?;

    // Process all of the event records.
    let mut modified = false;
    let trace: Vec<u8> = process_event_records(trace, |event_record, parsed_bytes| {
        // This closure runs over every event record.

        // Count the event categories.
        if let Some(category_counter) = &mut category_counter {
            category_counter.increment_category(&event_record.category);
        }

        // Symbolize FIDL IPC traces.
        if let Some(symbolizer) = &symbolizer {
            symbolizer.symbolize_event_record(&event_record, parsed_bytes, &mut modified)
        } else {
            parsed_bytes
        }
    })?;

    // Check that the categories which were specified were actually captured.
    if let Some(category_counter) = &mut category_counter {
        if trace.is_empty() {
            ffx_bail!(
                "{}WARNING: The trace file is empty. Please verify that the input categories are valid. Input categories are: {:?}.{}",
                color::Fg(color::Yellow),
                category_counter.input_categories,
                style::Reset
            );
        }
        let invalid_category_list = category_counter.get_invalid_category_list();
        if !invalid_category_list.is_empty() {
            warning_list.push(format!(
                "{}WARNING: Categories {:?} were manually specified, but not found in the resulting trace. Check the spelling of your categories or the status of your trace provider{}",
                color::Fg(color::Yellow),
                invalid_category_list,
                style::Reset
            ));
        }
    }

    // Write the trace if there's a different output path specified or if the processing actually modified the trace.
    if trace_file.as_ref() != outfile.as_ref() || modified {
        std::fs::write(outfile, trace)?;
    }
    Ok(warning_list)
}

/// Implements FIDL method symbolization for IPC traces.
pub struct Symbolizer {
    ordinals: SymbolizationMap,
}

impl Symbolizer {
    pub fn new(context: &EnvironmentContext) -> Self {
        let ordinals = SymbolizationMap::from_context(context).unwrap_or_default();
        Self { ordinals }
    }

    /// Apply FIDL method name symbolization to an event record.
    /// If the record was modified then set the modified reference.
    fn symbolize_event_record(
        &self,
        event_record: &fxt::EventRecord,
        parsed_bytes: Vec<u8>,
        modified: &mut bool,
    ) -> Vec<u8> {
        if event_record.category.as_str() == "kernel:ipc" {
            for arg in &event_record.args {
                match arg {
                    Arg { name, value: ArgValue::Unsigned64(ord) }
                        if name.as_str() == ORDINAL_ARG_NAME
                            && self.ordinals.contains_key(*ord) =>
                    {
                        *modified = true;
                        return symbolize_fidl_call(
                            &parsed_bytes,
                            *ord,
                            self.ordinals.get(*ord).unwrap(),
                        )
                        .unwrap_or(parsed_bytes);
                    }
                    _ => continue,
                }
            }
        }
        parsed_bytes
    }
}

fn symbolize_fidl_call<'a>(bytes: &[u8], ordinal: u64, method: &'a str) -> Result<Vec<u8>> {
    let (_, mut raw_event_record) =
        RawEventRecord::parse(bytes).expect("Unable to parse event record");
    let mut new_args = vec![];
    for arg in &raw_event_record.args {
        if let &RawArgValue::Unsigned64(arg_value) = &arg.value {
            if arg_value == ordinal {
                let symbolized_arg = RawArg {
                    name: StringRef::Inline("method"),
                    value: RawArgValue::String(StringRef::Inline(method)),
                };
                new_args.push(symbolized_arg);
            }
        }
        new_args.push(arg.clone());
    }

    raw_event_record.args = new_args;
    raw_event_record.serialize().map_err(|e| anyhow!(e))
}

pub struct CategoryCounter {
    category_counter: HashMap<String, usize>,
    input_categories: Vec<String>,
}

impl CategoryCounter {
    pub fn new(input_categories: Vec<String>) -> Self {
        let mut category_counter = HashMap::new();
        for category in &input_categories {
            // Skip categories starting with '#' since they represent groups.
            // We don't check them because it's not explicitly specified.
            if !category.starts_with("#") {
                category_counter.insert(category.clone(), 0);
            }
        }
        Self { category_counter, input_categories }
    }

    fn increment_category(&mut self, category: &str) {
        *self.category_counter.entry(category.to_string()).or_insert(0) += 1;

        // The "kernel" category is a special meta category that enables all "kernel:*" categories.
        // If we see any "kernel:" category, also consider "kernel" to be seen.
        if category.starts_with("kernel:") {
            *self.category_counter.entry("kernel".into()).or_insert(0) += 1;
        }
    }

    fn get_invalid_category_list(&self) -> Vec<String> {
        self.category_counter
            .iter()
            .filter(|(_, &count)| count == 0)
            .map(|(category, _)| category.clone())
            .collect()
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[fuchsia::test]
    async fn test_verify_missing() {
        let mut counter =
            CategoryCounter::new(vec!["some".into(), "other".into(), "categories".into()]);
        counter.increment_category("some");
        counter.increment_category("other");
        let missing = counter.get_invalid_category_list();
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0], "categories");
    }

    #[fuchsia::test]
    async fn test_verify_kernel_category() {
        let mut counter = CategoryCounter::new(vec![
            "kernel".into(),
            "some".into(),
            "other".into(),
            "categories".into(),
        ]);
        counter.increment_category("some");
        counter.increment_category("other");
        counter.increment_category("categories");
        counter.increment_category("kernel:meta");
        assert!(counter.get_invalid_category_list().is_empty());
    }
}
