// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Result};
use errors::ffx_bail;
use ffx_config::EnvironmentContext;
use ffx_trace::SymbolizationMap;
use fxt::{
    Arg, ArgValue, ParseError, RawArg, RawArgValue, RawEventRecord, SessionParser, StringRef,
    TraceRecord,
};
use std::collections::HashMap;
use std::fs::File;
use std::io::{LineWriter, Write};
use termion::{color, style};

static ORDINAL_ARG_NAME: &str = "ordinal";

pub struct TraceProcessor {
    parser: SessionParser<std::io::Cursor<Vec<u8>>>,
    symbolizer: Option<Symbolizer>,
    event_per_category_counter: Option<CategoryCounter>,
}

impl TraceProcessor {
    pub fn new(trace_file: String) -> Result<Self> {
        let content = match std::fs::read(&trace_file) {
            Ok(content) => content,
            Err(e) => ffx_bail!("Failed to read the trace file: {}", e),
        };

        let parser = SessionParser::new(std::io::Cursor::new(content));

        Ok(Self { parser, symbolizer: None, event_per_category_counter: None })
    }

    pub fn with_symbolizer(&mut self, outfile: &str, context: &EnvironmentContext) {
        self.symbolizer = Some(Symbolizer::new(outfile, &context));
    }

    pub fn with_category_check(&mut self, input_categories: Vec<String>) {
        self.event_per_category_counter = Some(CategoryCounter::new(input_categories));
    }

    fn check_record(&mut self, record: Result<TraceRecord, ParseError>) -> Result<()> {
        if let Some(event_per_category_counter) = &mut self.event_per_category_counter {
            if let Ok(TraceRecord::Event(fxt::EventRecord { ref category, .. })) = record {
                event_per_category_counter.increment_category(&category);
            }
        }

        if let Some(symbolizer) = &mut self.symbolizer {
            let parsed_bytes = self.parser.parsed_bytes().to_owned();
            symbolizer.symbolize_and_write_record(record, parsed_bytes)?;
        }

        Ok(())
    }

    pub fn run(mut self) -> Result<Vec<String>> {
        let mut warning_list = Vec::new();
        match self.parser.next() {
            Some(record) => self.check_record(record)?,
            None => {
                if let Some(event_per_category_counter) = &mut self.event_per_category_counter {
                    ffx_bail!(
                        "{}WARNING: The trace file is empty. Please verify that the input categories are valid. Input categories are: {:?}.{}",
                        color::Fg(color::Yellow),
                        event_per_category_counter.input_categories,
                        style::Reset
                    );
                }
            }
        }

        while let Some(record) = self.parser.next() {
            self.check_record(record)?;
        }

        if let Some(event_per_category_counter) = &mut self.event_per_category_counter {
            let invalid_category_list = event_per_category_counter.get_invalid_category_list();
            if !invalid_category_list.is_empty() {
                warning_list.push(format!(
                "{}WARNING: Categories {:?} were manually specified, but not found in the resulting trace. Check the spelling of your categories or the status of your trace provider{}",
                color::Fg(color::Yellow),
                invalid_category_list,
                style::Reset
            ));
            }
        }
        Ok(warning_list)
    }
}

struct Symbolizer {
    output_file: LineWriter<File>,
    ordinals: SymbolizationMap,
}

impl Symbolizer {
    fn new(outfile: &str, context: &EnvironmentContext) -> Self {
        let file = std::fs::File::create(outfile).unwrap();
        let output_file: LineWriter<File> = LineWriter::new(file);
        let ordinals = SymbolizationMap::from_context(context).unwrap_or_default();
        Self { output_file: output_file, ordinals }
    }

    fn symbolize_and_write_record(
        &mut self,
        record: Result<TraceRecord, ParseError>,
        parsed_bytes: Vec<u8>,
    ) -> Result<()> {
        let mut parsed_bytes = parsed_bytes;
        self.output_file.write_all(match record {
            Ok(fxt::TraceRecord::Event(fxt::EventRecord { category, args, .. }))
                if category.as_str() == "kernel:ipc" =>
            {
                for arg in args {
                    match arg {
                        Arg { name, value: ArgValue::Unsigned64(ord) }
                            if name.as_str() == ORDINAL_ARG_NAME
                                && self.ordinals.contains_key(ord) =>
                        {
                            parsed_bytes = symbolize_fidl_call(
                                &parsed_bytes,
                                ord,
                                self.ordinals.get(ord).unwrap(),
                            )
                            .unwrap_or(parsed_bytes)
                        }
                        _ => continue,
                    }
                }
                &parsed_bytes
            }
            _ => &parsed_bytes,
        })?;
        Ok(())
    }
}

impl Drop for Symbolizer {
    fn drop(&mut self) {
        let _ = self.output_file.flush();
    }
}

pub fn symbolize_fidl_call<'a>(bytes: &[u8], ordinal: u64, method: &'a str) -> Result<Vec<u8>> {
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

struct CategoryCounter {
    category_counter: HashMap<String, usize>,
    input_categories: Vec<String>,
}

impl CategoryCounter {
    fn new(input_categories: Vec<String>) -> Self {
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
