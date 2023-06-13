// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::cml,
    crate::error::Error,
    crate::util,
    crate::util::write_depfile,
    std::fs,
    std::io::{BufRead, BufReader, Write},
    std::path::PathBuf,
    std::str::FromStr,
};

/// read in the provided list of json files, merge them, and pretty-print the merged result to
/// stdout if output is None or to the provided path if output is Some.
/// Files can be specified in `files`, or in the contents of `fromfile` as line-delimited paths,
/// or both.
/// JSON objects are merged recursively, and if two blobs set the same key an error is returned.
/// JSON arrays are appended together, with duplicate items being removed.
/// If a depfile is provided, also writes the files encountered to the depfile.
pub fn merge(
    mut files: Vec<PathBuf>,
    output: Option<PathBuf>,
    // If specified, this is a path to newline-delimited `files`
    fromfile: Option<PathBuf>,
    depfile: Option<PathBuf>,
) -> Result<(), Error> {
    if let Some(path) = &fromfile {
        let reader = BufReader::new(fs::File::open(path).map_err(|e| {
            Error::invalid_args(format!("Failed to open --fromfile \"{:?}\": {}", path, e))
        })?);
        for line in reader.lines() {
            match line {
                Ok(value) => files.push(PathBuf::from(value)),
                Err(e) => return Err(Error::invalid_args(format!("Invalid --fromfile: {}", e))),
            }
        }
    }
    if files.is_empty() {
        return Err(Error::invalid_args(format!("no files provided")));
    }

    // If given an aggregated CML file generated using GN metadata, merge all of the included
    // CML objects therein.
    let mut document = cml::parse_one_document(&"{}".to_string(), &PathBuf::from_str("").unwrap())?;
    for file in &files {
        let include_documents = util::read_cml_tolerate_gn_metadata(&file)?;
        for mut d in include_documents.into_iter() {
            document.merge_from(&mut d, &file)?;
        }
    }

    let json_str = serde_json::to_string_pretty(&document)?;
    if let Some(output_path) = &output {
        util::ensure_directory_exists(output_path)?;
        fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(output_path)?
            .write_all(json_str.as_bytes())?;
    } else {
        println!("{}", json_str);
    }

    // Write files to depfile
    if let Some(depfile_path) = depfile {
        write_depfile(&depfile_path, output.as_ref(), &files)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs::File;
    use std::io::{LineWriter, Read, Write};
    use tempfile::TempDir;

    #[test]
    fn test_merge_cml() {
        let tmp_dir = TempDir::new().unwrap();

        let input = vec![
            (tmp_dir.path().join("1.cml"), "{use: [{ protocol: [\"bar\"]}]}"),
            (tmp_dir.path().join("2.cml"), "{use: [{ protocol: [\"foo\", \"bar\"]}]}"),
        ];
        let mut filenames = vec![];
        for (fname, contents) in &input {
            File::create(fname).unwrap().write_all(contents.as_bytes()).unwrap();
            filenames.push(fname.clone());
        }

        let output_file_path = tmp_dir.path().join("output.json");
        merge(filenames, Some(output_file_path.clone()), None, None).expect("failed to merge");

        let mut buffer = String::new();
        File::open(&output_file_path).unwrap().read_to_string(&mut buffer).unwrap();
        let expected_json = json!({
            "use": [
                {"protocol": ["bar"]},
                {"protocol": "foo"},
            ]
        });
        assert_eq!(buffer, format!("{:#}", expected_json));
    }

    #[test]
    fn test_merge_cml_gn_metadata() {
        let tmp_dir = TempDir::new().unwrap();

        let input = vec![
            (
                tmp_dir.path().join("1.cml"),
                "[{use: [{ protocol: [\"bar\"]}]}, {use: [{ protocol: [\"baz\"]}]}]",
            ),
            (tmp_dir.path().join("2.cml"), "[{use: [{ protocol: [\"foo\", \"bar\"]}]}]"),
        ];
        let mut filenames = vec![];
        for (fname, contents) in &input {
            File::create(fname).unwrap().write_all(contents.as_bytes()).unwrap();
            filenames.push(fname.clone());
        }

        let output_file_path = tmp_dir.path().join("output.json");
        merge(filenames, Some(output_file_path.clone()), None, None).expect("failed to merge");

        let mut buffer = String::new();
        File::open(&output_file_path).unwrap().read_to_string(&mut buffer).unwrap();
        let expected_json = json!({
            "use": [
                {"protocol": ["bar"]},
                {"protocol": ["baz"]},
                {"protocol": "foo"},
            ]
        });
        assert_eq!(buffer, format!("{:#}", expected_json));
    }

    #[test]
    fn test_merge_invalid_json_fails() {
        let tmp_dir = TempDir::new().unwrap();

        let input = vec![
            (tmp_dir.path().join("1.json"), "{\"foo\": 1}"),
            (tmp_dir.path().join("2.json"), "{\"foo\": 1,}"),
        ];
        let mut filenames = vec![];
        for (fname, contents) in &input {
            File::create(fname).unwrap().write_all(contents.as_bytes()).unwrap();
            filenames.push(fname.clone());
        }

        let result = merge(filenames, None, None, None);
        assert!(result.is_err());
    }

    #[test]
    fn test_merge_fromfile() {
        let tmp_dir = TempDir::new().unwrap();

        let input = vec![
            // The first two files will be provided as regular inputs
            (tmp_dir.path().join("1.cml"), "{use: [{ protocol: [\"bar\"]}]}"),
            (tmp_dir.path().join("2.cml"), "{use: [{ protocol: [\"foo\"]}]}"),
            // The third file will be referenced via --fromfile
            (tmp_dir.path().join("3.cml"), "{use: [{ protocol: [\"baz\"]}]}"),
        ];
        for (fname, contents) in &input {
            File::create(fname).unwrap().write_all(contents.as_bytes()).unwrap();
        }
        let mut filenames = vec![];
        for (fname, _) in &input[..2] {
            filenames.push(fname.clone());
        }
        let fromfile_path = tmp_dir.path().join("fromfile");
        let mut fromfile = LineWriter::new(File::create(fromfile_path.clone()).unwrap());
        writeln!(fromfile, "{}", input[2].0.clone().into_os_string().into_string().unwrap())
            .unwrap();

        let output_file_path = tmp_dir.path().join("output.cml");
        merge(filenames, Some(output_file_path.clone()), Some(fromfile_path), None)
            .expect("failed to merge");

        let mut buffer = String::new();
        File::open(&output_file_path).unwrap().read_to_string(&mut buffer).unwrap();
        let expected_json = json!({
            "use": [
                {"protocol": ["bar"]},
                {"protocol": ["foo"]},
                {"protocol": ["baz"]},
            ]
        });
        assert_eq!(buffer, format!("{:#}", expected_json));
    }
}
