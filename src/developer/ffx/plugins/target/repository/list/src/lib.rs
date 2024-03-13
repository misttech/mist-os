// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Result};
use async_trait::async_trait;
use ffx_target_repository_list_args::ListCommand;
use fho::{daemon_protocol, FfxMain, FfxTool, ToolIO, VerifiedMachineWriter};
use fidl_fuchsia_developer_ffx::{RepositoryRegistryProxy, RepositoryStorageType};
use prettytable::{cell, row, Table};
use std::collections::HashMap;

#[derive(FfxTool)]
pub struct ListTool {
    #[command]
    cmd: ListCommand,
    #[with(daemon_protocol())]
    repos: RepositoryRegistryProxy,
}

type RepositoryList = Vec<(String, Vec<String>)>;
type Writer = VerifiedMachineWriter<RepositoryList>;

fho::embedded_plugin!(ListTool);
#[async_trait(?Send)]
impl FfxMain for ListTool {
    type Writer = Writer;
    async fn main(self, writer: Self::Writer) -> fho::Result<()> {
        list_impl(self.cmd, self.repos, writer).await?;
        Ok(())
    }
}

async fn list_impl(
    _cmd: ListCommand,
    repos: RepositoryRegistryProxy,
    mut writer: VerifiedMachineWriter<RepositoryList>,
) -> Result<()> {
    let (client, server) = fidl::endpoints::create_endpoints();
    repos.list_registered_targets(server).context("communicating with daemon")?;
    let registered_targets = client.into_proxy()?;

    let mut items = HashMap::new();

    while let Some(registered_targets) =
        registered_targets.next().await.map(|x| if x.is_empty() { None } else { Some(x) })?
    {
        for registered_target in registered_targets {
            let repo = registered_target.repo_name.unwrap_or("<unknown>".to_owned());
            let mut target_identifier =
                registered_target.target_identifier.unwrap_or("<unknown>".to_owned());

            if registered_target.storage_type == Some(RepositoryStorageType::Ephemeral) {
                target_identifier.push_str(" (EPHEMERAL)")
            }

            let mut aliases = registered_target.aliases.unwrap_or_else(Vec::new);
            aliases.sort();

            for alias in aliases {
                target_identifier.push_str(&format!("\n  alias: {}", alias));
            }

            items.entry(repo).or_insert_with(Vec::new).push(target_identifier);
        }
    }

    if items.is_empty() {
        if writer.is_machine() {
            return Ok(writer.machine(&vec![])?);
        }

        return Ok(());
    }

    for value in items.values_mut() {
        value.sort();
    }

    let mut items = items.into_iter().collect::<Vec<_>>();
    items.sort_by(|(x, _), (y, _)| x.cmp(&y));

    if writer.is_machine() {
        return Ok(writer.machine(&items)?);
    }

    let mut table = Table::new();
    table.set_titles(row!("REPO", "TARGET"));

    for (repo, targets) in items {
        table.add_row(row!(repo, targets.join("\n"),));
    }

    table.print(&mut writer)?;

    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    use ffx_writer::Format;
    use fho::macro_deps::ffx_writer::TestBuffers;
    use fidl_fuchsia_developer_ffx::{
        RepositoryRegistryRequest, RepositoryTarget, RepositoryTargetsIteratorRequest,
    };
    use fuchsia_async as fasync;
    use futures::StreamExt;

    async fn empty_repo_proxy() -> RepositoryRegistryProxy {
        fho::testing::fake_proxy(move |req| {
            fasync::Task::spawn(async move {
                match req {
                    RepositoryRegistryRequest::ListRegisteredTargets { iterator, .. } => {
                        let mut iterator = iterator.into_stream().unwrap();
                        while let Some(Ok(req)) = iterator.next().await {
                            match req {
                                RepositoryTargetsIteratorRequest::Next { responder } => {
                                    responder.send(&[]).unwrap()
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

    async fn repo_proxy() -> RepositoryRegistryProxy {
        fho::testing::fake_proxy(move |req| {
            fasync::Task::spawn(async move {
                let mut sent = false;
                match req {
                    RepositoryRegistryRequest::ListRegisteredTargets { iterator, .. } => {
                        let mut iterator = iterator.into_stream().unwrap();
                        while let Some(Ok(req)) = iterator.next().await {
                            match req {
                                RepositoryTargetsIteratorRequest::Next { responder } => {
                                    if !sent {
                                        sent = true;
                                        responder
                                            .send(&[
                                                RepositoryTarget {
                                                    repo_name: Some("bob".to_owned()),
                                                    target_identifier: Some("target1".to_owned()),
                                                    aliases: Some(vec![
                                                        "target1_alias1".to_owned(),
                                                        "target1_alias2".to_owned(),
                                                    ]),
                                                    ..Default::default()
                                                },
                                                RepositoryTarget {
                                                    repo_name: Some("smith".to_owned()),
                                                    target_identifier: Some("target2".to_owned()),
                                                    storage_type: Some(
                                                        RepositoryStorageType::Ephemeral,
                                                    ),
                                                    ..Default::default()
                                                },
                                                RepositoryTarget {
                                                    repo_name: Some("bob".to_owned()),
                                                    target_identifier: Some("target3".to_owned()),
                                                    ..Default::default()
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

    #[fasync::run_singlethreaded(test)]
    async fn list_table() {
        let repos = repo_proxy().await;
        let test_buffers = TestBuffers::default();
        let writer = Writer::new_test(None, &test_buffers);
        list_impl(ListCommand {}, repos, writer).await.unwrap();

        static EXPECT: &str = "\
             +-------+-------------------------+\n\
             | REPO  | TARGET                  |\n\
             +=======+=========================+\n\
             | bob   | target1                 |\n\
             |       |   alias: target1_alias1 |\n\
             |       |   alias: target1_alias2 |\n\
             |       | target3                 |\n\
             +-------+-------------------------+\n\
             | smith | target2 (EPHEMERAL)     |\n\
             +-------+-------------------------+\n";

        assert_eq!(EXPECT, test_buffers.into_stdout_str());
    }

    #[fasync::run_singlethreaded(test)]
    async fn list_json() {
        let repos = repo_proxy().await;
        let test_buffers = TestBuffers::default();
        let writer = Writer::new_test(Some(Format::Json), &test_buffers);
        list_impl(ListCommand {}, repos, writer).await.unwrap();

        static EXPECT: &str = "[[\"bob\",[\"target1\\n  alias: target1_alias1\\n  alias: target1_alias2\",\"target3\"]],[\"smith\",[\"target2 (EPHEMERAL)\"]]]\n";
        assert_eq!(EXPECT, (test_buffers.into_stdout_str()));
    }

    #[fasync::run_singlethreaded(test)]
    async fn list_empty() {
        let repos = empty_repo_proxy().await;
        let test_buffers = TestBuffers::default();
        let writer = Writer::new_test(None, &test_buffers);
        list_impl(ListCommand {}, repos, writer).await.unwrap();

        static EXPECT: &str = "";
        assert_eq!(EXPECT, (test_buffers.into_stdout_str()));
    }

    #[fasync::run_singlethreaded(test)]
    async fn list_json_empty() {
        let repos = empty_repo_proxy().await;
        let test_buffers = TestBuffers::default();
        let writer = Writer::new_test(Some(Format::Json), &test_buffers);
        list_impl(ListCommand {}, repos, writer).await.unwrap();

        static EXPECT: &str = "[]\n";
        assert_eq!(EXPECT, (test_buffers.into_stdout_str()));
    }
}
