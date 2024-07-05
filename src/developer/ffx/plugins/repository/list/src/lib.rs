// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ffx_repository_list_args::ListCommand;
use fho::{bug, daemon_protocol, FfxMain, FfxTool, MachineWriter, Result, ToolIO as _};
use fidl_fuchsia_developer_ffx::{RepositoryIteratorMarker, RepositoryRegistryProxy};
use fidl_fuchsia_developer_ffx_ext::{RepositoryConfig, RepositorySpec};
use prettytable::format::TableFormat;
use prettytable::{cell, row, table, Cell, Table};
use std::collections::BTreeSet;

#[derive(FfxTool)]
pub struct RepoListTool {
    #[command]
    pub cmd: ListCommand,
    #[with(daemon_protocol())]
    repos: RepositoryRegistryProxy,
}

fho::embedded_plugin!(RepoListTool);

#[async_trait::async_trait(?Send)]
impl FfxMain for RepoListTool {
    type Writer = MachineWriter<Vec<RepositoryConfig>>;
    async fn main(self, mut writer: Self::Writer) -> Result<()> {
        list_impl(self.cmd, self.repos, None, &mut writer).await
    }
}

async fn list_impl(
    _cmd: ListCommand,
    repos_proxy: RepositoryRegistryProxy,
    table_format: Option<TableFormat>,
    writer: &mut <RepoListTool as FfxMain>::Writer,
) -> Result<()> {
    let (client, server) = fidl::endpoints::create_endpoints::<RepositoryIteratorMarker>();
    repos_proxy.list_repositories(server).map_err(|e| bug!("error listing repositories: {e}"))?;
    let client =
        client.into_proxy().map_err(|e| bug!("error creating repository iterator proxy: {e}"))?;

    let default_repo = pkg::config::get_default_repository()
        .await
        .map_err(|e| bug!("error getting default repository: {e}"))?;

    let mut repos = vec![];
    loop {
        let batch = client
            .next()
            .await
            .map_err(|e| bug!("error fetching next batch of repositories: {e}"))?;
        if batch.is_empty() {
            break;
        }

        for repo in batch {
            repos
                .push(repo.try_into().map_err(|e| bug!("error converting repository config {e}"))?);
        }
    }

    repos.sort();

    if writer.is_machine() {
        writer
            .machine(&repos)
            .map_err(|e| bug!("error writing machine representation of repositories {e}"))?;
    } else {
        print_table(&repos, default_repo, table_format, writer)
            .map_err(|e| bug!("error printing repository table {e}"))?
    }

    Ok(())
}

fn print_table(
    repos: &[RepositoryConfig],
    default_repo: Option<String>,
    table_format: Option<TableFormat>,
    writer: &mut <RepoListTool as FfxMain>::Writer,
) -> Result<()> {
    let mut table = Table::new();
    table.set_titles(row!("NAME", "TYPE", "ALIASES", "EXTRA"));
    if let Some(fmt) = table_format {
        table.set_format(fmt);
    }

    let mut rows = vec![];

    for repo in repos {
        let mut row = row!();

        if default_repo.as_deref() == Some(&repo.name) {
            row.add_cell(cell!(format!("{}*", repo.name)));
        } else {
            row.add_cell(cell!(repo.name));
        }

        match &repo.spec {
            RepositorySpec::FileSystem { metadata_repo_path, blob_repo_path, aliases } => {
                row.add_cell(cell!("filesystem"));
                row.add_cell(cell_for_aliases(aliases));
                row.add_cell(cell!(table!(
                    ["metadata", metadata_repo_path],
                    ["blobs", blob_repo_path]
                )));
            }
            RepositorySpec::Pm { path, aliases } => {
                row.add_cell(cell!("pm"));
                row.add_cell(cell_for_aliases(aliases));
                row.add_cell(cell!(path));
            }
            RepositorySpec::Http { metadata_repo_url, blob_repo_url, aliases } => {
                row.add_cell(cell!("http"));
                row.add_cell(cell_for_aliases(aliases));
                row.add_cell(cell!(table!(
                    ["metadata", metadata_repo_url],
                    ["blobs", blob_repo_url]
                )));
            }
            RepositorySpec::Gcs { metadata_repo_url, blob_repo_url, aliases } => {
                row.add_cell(cell!("gcs"));
                row.add_cell(cell_for_aliases(aliases));
                row.add_cell(cell!(table!(
                    ["metadata", metadata_repo_url],
                    ["blobs", blob_repo_url]
                )));
            }
        }

        rows.push(row);
    }

    for row in rows.into_iter() {
        table.add_row(row);
    }

    table.print(writer).map_err(|e| bug!("error printing table to writer: {e}"))?;

    return Ok(());
}

fn cell_for_aliases(aliases: &BTreeSet<String>) -> Cell {
    if aliases.is_empty() {
        cell!("")
    } else {
        cell!(aliases.iter().map(|alias| row!(alias)).collect::<Table>())
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use fho::{Format, TestBuffers};
    use fidl_fuchsia_developer_ffx::{
        FileSystemRepositorySpec, PmRepositorySpec, RepositoryConfig, RepositoryIteratorRequest,
        RepositoryRegistryRequest, RepositorySpec,
    };
    use fuchsia_async as fasync;
    use futures::StreamExt;
    use pretty_assertions::assert_eq;

    fn fake_repos() -> RepositoryRegistryProxy {
        fho::testing::fake_proxy(move |req| {
            fasync::Task::spawn(async move {
                let mut sent = false;
                match req {
                    RepositoryRegistryRequest::ListRepositories { iterator, .. } => {
                        let mut iterator = iterator.into_stream().unwrap();
                        while let Some(Ok(req)) = iterator.next().await {
                            match req {
                                RepositoryIteratorRequest::Next { responder } => {
                                    if !sent {
                                        sent = true;
                                        responder
                                            .send(&[
                                                RepositoryConfig {
                                                    name: "Test1".to_owned(),
                                                    spec: RepositorySpec::FileSystem(
                                                        FileSystemRepositorySpec {
                                                            metadata_repo_path: Some(
                                                                "a/b/meta".to_owned(),
                                                            ),
                                                            blob_repo_path: Some(
                                                                "a/b/blobs".to_owned(),
                                                            ),
                                                            ..Default::default()
                                                        },
                                                    ),
                                                },
                                                RepositoryConfig {
                                                    name: "Test2".to_owned(),
                                                    spec: RepositorySpec::Pm(PmRepositorySpec {
                                                        path: Some("c/d".to_owned()),
                                                        aliases: Some(vec![
                                                            "example.com".into(),
                                                            "fuchsia.com".into(),
                                                        ]),
                                                        ..Default::default()
                                                    }),
                                                },
                                            ])
                                            .unwrap()
                                    } else {
                                        responder.send(&[]).unwrap()
                                    }
                                }
                            }
                        }
                    }
                    other => panic!("Unexpected request: {:?}", other),
                }
            })
            .detach();
        })
    }

    #[fuchsia::test]
    async fn test_list() {
        let _env = ffx_config::test_init().await.unwrap();
        let repos = fake_repos();

        let buffers = TestBuffers::default();
        let mut out = MachineWriter::new_test(None, &buffers);
        list_impl(ListCommand {}, repos, None, &mut out).await.unwrap();

        assert_eq!(
            buffers.into_stdout_str(),
            "\
            +-------+------------+-----------------+--------------------------+\n\
            | NAME  | TYPE       | ALIASES         | EXTRA                    |\n\
            +=======+============+=================+==========================+\n\
            | Test1 | filesystem |                 | +----------+-----------+ |\n\
            |       |            |                 | | metadata | a/b/meta  | |\n\
            |       |            |                 | +----------+-----------+ |\n\
            |       |            |                 | | blobs    | a/b/blobs | |\n\
            |       |            |                 | +----------+-----------+ |\n\
            +-------+------------+-----------------+--------------------------+\n\
            | Test2 | pm         | +-------------+ | c/d                      |\n\
            |       |            | | example.com | |                          |\n\
            |       |            | +-------------+ |                          |\n\
            |       |            | | fuchsia.com | |                          |\n\
            |       |            | +-------------+ |                          |\n\
            +-------+------------+-----------------+--------------------------+\n",
        );
    }

    #[fuchsia::test]
    async fn test_machine() {
        let _env = ffx_config::test_init().await.unwrap();
        let repos = fake_repos();
        let buffers = TestBuffers::default();
        let mut out = MachineWriter::new_test(Some(Format::Json), &buffers);
        list_impl(ListCommand {}, repos, None, &mut out).await.unwrap();

        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&buffers.into_stdout_str()).unwrap(),
            serde_json::json!([
                {
                    "name": "Test1",
                    "spec": {
                        "type": "file_system",
                        "metadata_repo_path": "a/b/meta",
                        "blob_repo_path": "a/b/blobs",
                    },
                },
                {
                    "name": "Test2",
                    "spec": {
                        "type": "pm",
                        "path": "c/d",
                        "aliases": ["example.com", "fuchsia.com"],
                    },
                },
            ]),
        );
    }
}
