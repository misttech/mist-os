// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use component_id_index::{gen_instance_id, Index, InstanceIdEntry, MergeContext, ValidationError};
use fidl::persist;
use fidl_fuchsia_component_internal::ComponentIdIndex;
use std::fs::File;

/// A builder that constructs component id indices.
#[derive(Default)]
pub struct ComponentIdIndexBuilder {
    indices: Vec<Utf8PathBuf>,
}

impl ComponentIdIndexBuilder {
    /// Add a product-provided index.
    pub fn index(&mut self, index: impl AsRef<Utf8Path>) -> &mut Self {
        self.indices.push(index.as_ref().into());
        self
    }

    /// Build the final index by merging all the indices from the product and
    /// optionally the core component id index.
    pub fn build(self, outdir: impl AsRef<Utf8Path>) -> Result<Utf8PathBuf> {
        let merged_index =
            merge_index_from_json5_files(&self.indices).context("merging index files")?;
        let merged_index_fidl: ComponentIdIndex =
            merged_index.try_into().context("converting index into fidl")?;
        let bytes = persist(&merged_index_fidl).context("Could not fidl-encode index")?;

        let path = outdir.as_ref().join("component_id_index.fidlbin");
        std::fs::write(&path, bytes)
            .context("Could not write merged FIDL-encoded index to file")?;
        Ok(path)
    }
}

// Make an Index using a set of JSON5-encoded index files.
fn merge_index_from_json5_files(index_files: &[Utf8PathBuf]) -> Result<Index> {
    let mut ctx = MergeContext::new();
    for path in index_files {
        let mut file = File::open(&path).with_context(|| format!("opening {path}"))?;
        let index: Index =
            serde_json5::from_reader(&mut file).with_context(|| format!("parsing {path}"))?;
        ctx.merge(path.as_str(), &index)
            .with_context(|| format!("merging {path}"))
            .map_err(|e| {
                match e.downcast_ref::<ValidationError>() {
                    Some(ValidationError::MissingInstanceIds{entries}) => {
                        let corrected_entries = generate_instance_ids(&entries);
                        anyhow!("Some entries are missing `instance_id` fields. Here are some generated IDs for you:\n{}\n\nSee https://fuchsia.dev/fuchsia-src/development/components/component_id_index#defining_an_index for more details.",
                                serde_json::to_string_pretty(&corrected_entries).unwrap())
                    },
                    _ => e
                }
            })?;
    }

    Ok(ctx.output())
}

// Generate a random instance id for every provided entry, and return a new
// list of entries. This is used to provide the user with an actionable error
// message containing instance ids they can use.
fn generate_instance_ids(entries: &Vec<InstanceIdEntry>) -> Vec<InstanceIdEntry> {
    let rng = &mut rand::thread_rng();
    (0..entries.len())
        .map(|i| {
            let mut with_id = entries[i].clone();
            with_id.instance_id = Some(gen_instance_id(rng));
            with_id
        })
        .collect::<Vec<InstanceIdEntry>>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use component_id_index::InstanceIdEntry;
    use moniker::{Moniker, MonikerBase};
    use pretty_assertions::assert_eq;
    use routing::component_id_index::{ComponentIdIndex, ComponentInstanceId};
    use std::io::Write;
    use std::str::FromStr;

    fn gen_index(start: u32, end: u32) -> Index {
        Index {
            instances: (start..end)
                .map(|i| InstanceIdEntry {
                    instance_id: Some(format!(
                        "00000000000000000000000000000000000000000000000000000000000000{:02x}",
                        i
                    )),
                    moniker: Some(Moniker::parse_str(&format!("/a/b/c/{i}")).unwrap()),
                })
                .collect(),
        }
    }

    fn make_index_file(index: &Index) -> anyhow::Result<tempfile::NamedTempFile> {
        let mut index_file = tempfile::NamedTempFile::new()?;
        index_file.write_all(serde_json::to_string(index).unwrap().as_bytes())?;
        Ok(index_file)
    }

    #[test]
    fn error_missing_instance_ids() {
        let mut index = gen_index(0, 4);
        index.instances[1].instance_id = None;
        index.instances[3].instance_id = None;

        let index_file = make_index_file(&index).unwrap();
        let index_files = [Utf8PathBuf::from_path_buf(index_file.path().to_path_buf()).unwrap()];

        // this should be an error, since `index` has entries with a missing instance ID.
        // check the error output's message as well.
        let merge_result = merge_index_from_json5_files(&index_files);
        let actual_output = merge_result.err().unwrap().to_string();
        let expected_output = r#"Some entries are missing `instance_id` fields. Here are some generated IDs for you:
[
  {
    "instance_id": "RANDOM_GENERATED_INSTANCE_ID",
    "moniker": "a/b/c/1"
  },
  {
    "instance_id": "RANDOM_GENERATED_INSTANCE_ID",
    "moniker": "a/b/c/3"
  }
]

See https://fuchsia.dev/fuchsia-src/development/components/component_id_index#defining_an_index for more details."#;

        let re = regex::Regex::new("[0-9a-f]{64}").unwrap();
        let actual_output_modified = re.replace_all(&actual_output, "RANDOM_GENERATED_INSTANCE_ID");
        assert_eq!(actual_output_modified, expected_output);
    }

    #[test]
    fn index_from_json5() {
        let mut index_file = tempfile::NamedTempFile::new().unwrap();
        index_file
            .write_all(
                r#"{
            // Here is a comment.
            instances: []
        }"#
                .as_bytes(),
            )
            .unwrap();

        // only checking that we parsed successfully.
        let files = [Utf8PathBuf::from_path_buf(index_file.path().to_path_buf()).unwrap()];
        assert!(merge_index_from_json5_files(&files).is_ok());
    }

    #[test]
    fn build_empty() {
        let outdir = tempfile::TempDir::new().unwrap();
        let outdir_path = Utf8PathBuf::from_path_buf(outdir.path().to_path_buf()).unwrap();
        let builder = ComponentIdIndexBuilder::default();
        let path = builder.build(&outdir_path).unwrap();
        assert_eq!(path, outdir_path.join("component_id_index.fidlbin"));
    }

    #[test]
    fn build_multiple_indices() {
        let mut tmp_input_index1 = tempfile::NamedTempFile::new().unwrap();
        let mut tmp_input_index2 = tempfile::NamedTempFile::new().unwrap();

        // write the first index file
        let index1 = gen_index(0, 2);
        let index1_path =
            Utf8PathBuf::from_path_buf(tmp_input_index1.path().to_path_buf()).unwrap();
        tmp_input_index1.write_all(serde_json5::to_string(&index1).unwrap().as_bytes()).unwrap();

        // write the second index file
        let index2 = gen_index(2, 4);
        let index2_path =
            Utf8PathBuf::from_path_buf(tmp_input_index2.path().to_path_buf()).unwrap();
        tmp_input_index2.write_all(serde_json5::to_string(&index2).unwrap().as_bytes()).unwrap();

        let outdir = tempfile::TempDir::new().unwrap();
        let outdir_path = Utf8PathBuf::from_path_buf(outdir.path().to_path_buf()).unwrap();
        let mut builder = ComponentIdIndexBuilder::default();
        builder.index(&index1_path);
        builder.index(&index2_path);
        let path = builder.build(&outdir_path).unwrap();
        assert_eq!(path, outdir_path.join("component_id_index.fidlbin"));
        let index = ComponentIdIndex::new(path.as_str()).unwrap();

        for i in 0..4 {
            let mon = Moniker::parse_str(&format!("/a/b/c/{}", i)).unwrap();
            let instance = index.look_up_moniker(&mon).unwrap();
            let expected = ComponentInstanceId::from_str(&format!(
                "00000000000000000000000000000000000000000000000000000000000000{:02x}",
                i
            ))
            .unwrap();
            assert!(index.look_up_instance_id(&expected));
            assert_eq!(&expected, instance);
        }
    }
}
