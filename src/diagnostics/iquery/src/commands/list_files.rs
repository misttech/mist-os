// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        commands::{
            constants::{IQUERY_TIMEOUT, IQUERY_TIMEOUT_SECS},
            types::*,
            utils::*,
        },
        types::Error,
    },
    argh::FromArgs,
    async_trait::async_trait,
    component_debug::dirs::*,
    fidl_fuchsia_io as fio, fidl_fuchsia_sys2 as fsys2,
    futures::StreamExt,
    lazy_static::lazy_static,
    regex::Regex,
    serde::Serialize,
    std::{cmp::Ordering, fmt},
};

lazy_static! {
    static ref MATCHING_REGEX: &'static str =
        r"^((.*\.inspect)|(fuchsia.inspect.Tree)|(fuchsia.inspect.deprecated.Inspect))$";
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ListFilesResultItem {
    pub(crate) moniker: String,
    pub(crate) files: Vec<String>,
}

impl ListFilesResultItem {
    pub fn new(moniker: String, files: Vec<String>) -> Self {
        ListFilesResultItem { moniker, files }
    }
}

impl PartialOrd for ListFilesResultItem {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ListFilesResultItem {
    fn cmp(&self, other: &Self) -> Ordering {
        self.moniker.cmp(&other.moniker)
    }
}

#[derive(Serialize)]
pub struct ListFilesResult(Vec<ListFilesResultItem>);

impl ListFilesResult {
    pub fn into_inner(self) -> Vec<ListFilesResultItem> {
        self.0
    }
}

impl fmt::Display for ListFilesResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for item in self.0.iter() {
            writeln!(f, "{}:", item.moniker)?;
            for file in item.files.iter() {
                writeln!(f, "  {}", file)?;
            }
        }
        Ok(())
    }
}

/// Lists all inspect files (*inspect vmo files, fuchsia.inspect.Tree and
/// fuchsia.inspect.deprecated.Inspect) under the provided paths. If no monikers are provided, it'll
/// list all the inspect files for all components.
#[derive(Default, FromArgs, PartialEq, Debug)]
#[argh(subcommand, name = "list-files")]
pub struct ListFilesCommand {
    #[argh(positional)]
    /// monikers to query on.
    pub monikers: Vec<String>,
}

#[async_trait]
impl Command for ListFilesCommand {
    type Result = ListFilesResult;

    async fn execute<P: DiagnosticsProvider>(&self, provider: &P) -> Result<Self::Result, Error> {
        provider.list_files(&self.monikers).await.map(ListFilesResult)
    }
}

async fn recursive_list_inspect_files(proxy: fio::DirectoryProxy) -> Vec<String> {
    let expected_accessor_re = Regex::new(&MATCHING_REGEX).unwrap();
    fuchsia_fs::directory::readdir_recursive(&proxy, Some(IQUERY_TIMEOUT.into()))
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .filter_map(|entry| match entry {
            Ok(ref dir_entry) => {
                if (dir_entry.kind == fuchsia_fs::directory::DirentKind::File
                    || dir_entry.kind == fuchsia_fs::directory::DirentKind::Service)
                    && expected_accessor_re.is_match(&dir_entry.name)
                {
                    Some(String::from(&dir_entry.name))
                } else {
                    None
                }
            }
            Err(fuchsia_fs::directory::RecursiveEnumerateError::Timeout) => {
                eprintln!(
                    "Warning: Read directory timed out after {} second(s)",
                    IQUERY_TIMEOUT_SECS,
                );
                None
            }
            Err(_) => None,
        })
        .collect::<Vec<_>>()
}

pub async fn list_files(
    realm_query_proxy: fsys2::RealmQueryProxy,
    monikers: &[String],
) -> Result<Vec<ListFilesResultItem>, Error> {
    let monikers = if monikers.is_empty() {
        get_instance_infos(&realm_query_proxy)
            .await?
            .iter()
            .map(|e| e.moniker.to_string())
            .collect::<Vec<_>>()
    } else {
        let mut processed = vec![];
        for moniker in monikers.iter() {
            processed.push(normalize_moniker(moniker));
        }
        processed
    };

    let mut output_vec = vec![];

    for moniker in &monikers {
        let relative_moniker = moniker.as_str().try_into().unwrap();
        let result = open_instance_dir_root_readable(
            &relative_moniker,
            OpenDirType::Outgoing,
            &realm_query_proxy,
        )
        .await;

        let out_dir_proxy = match result {
            Ok(out_dir_proxy) => out_dir_proxy,
            Err(OpenError::InstanceNotFound(_)) => {
                return Err(Error::InvalidComponent(moniker.to_string()))
            }
            Err(_) => continue,
        };

        // `fuchsia_fs::directory::open_directory` could block forever when the directory
        // it is trying to open does not exist.
        // TODO(https://fxbug.dev/110964): use `open_directory` hang bug is fixed.
        let diagnostics_dir_proxy = match fuchsia_fs::directory::open_directory_no_describe(
            &out_dir_proxy,
            "diagnostics",
            fuchsia_fs::OpenFlags::RIGHT_READABLE,
        ) {
            Ok(p) => p,
            Err(_) => continue,
        };

        let mut files = recursive_list_inspect_files(diagnostics_dir_proxy).await;
        files.sort();

        if files.len() > 0 {
            output_vec.push(ListFilesResultItem { moniker: normalize_moniker(moniker), files })
        }
    }

    output_vec.sort();
    Ok(output_vec)
}
