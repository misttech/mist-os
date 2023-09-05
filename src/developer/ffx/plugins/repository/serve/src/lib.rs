// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::anyhow,
    async_trait::async_trait,
    camino::Utf8Path,
    ffx_config::{keys::TARGET_DEFAULT_KEY, EnvironmentContext},
    ffx_repository_serve_args::ServeCommand,
    fho::{moniker, AvailabilityFlag, FfxContext, FfxMain, FfxTool, Result, SimpleWriter},
    fidl_fuchsia_developer_ffx::{
        RepositoryError as FfxCliRepositoryError,
        RepositoryStorageType as FfxCliRepositoryStorageType,
        RepositoryTarget as FfxCliRepositoryTarget,
    },
    fidl_fuchsia_developer_ffx::{TargetInfo, TargetProxy},
    fidl_fuchsia_developer_ffx_ext::RepositoryTarget as FfxDaemonRepositoryTarget,
    fidl_fuchsia_pkg::RepositoryManagerProxy,
    fidl_fuchsia_pkg_rewrite::EngineProxy,
    fuchsia_async as fasync,
    fuchsia_repo::{
        manager::RepositoryManager, repo_client::RepoClient, repository::PmRepository,
        server::RepositoryServer,
    },
    pkg::repo::register_target_with_fidl_proxies,
    std::{fs, io::Write, sync::Arc, time::Duration},
    timeout::timeout,
};

const REPO_FOREGROUND_FEATURE_FLAG: &str = "repository.foreground.enabled";

#[derive(FfxTool)]
#[check(AvailabilityFlag(REPO_FOREGROUND_FEATURE_FLAG))]
pub struct ServeTool {
    #[command]
    cmd: ServeCommand,
    context: EnvironmentContext,
    target_proxy: TargetProxy,
    #[with(moniker("/core/pkg-resolver"))]
    repo_proxy: RepositoryManagerProxy,
    #[with(moniker("/core/pkg-resolver"))]
    rewrite_engine_proxy: EngineProxy,
}

fho::embedded_plugin!(ServeTool);

#[async_trait(?Send)]
impl FfxMain for ServeTool {
    type Writer = SimpleWriter;

    async fn main(self, mut writer: SimpleWriter) -> Result<()> {
        let target: TargetInfo = timeout(Duration::from_secs(1), self.target_proxy.identity())
            .await
            .user_message("Timed out getting target identity")?
            .user_message("Failed to get target identity")?;

        let repo_name = self.cmd.repository;
        let repo_server_listen_addr = self.cmd.address;

        let repo_path = if let Some(repo_path) = self.cmd.repo_path {
            repo_path
        } else {
            // Default to "FUCHSIA_BUILD_DIR/amber-files"
            let fuchsia_build_dir = self
                .context
                .build_dir()
                .map(Utf8Path::from_path)
                .flatten()
                .unwrap_or(Utf8Path::new(""));
            let repo_path = format!("{}/amber-files", fuchsia_build_dir);

            repo_path
        };

        let target_identifier: String = self
            .context
            .query(TARGET_DEFAULT_KEY)
            .get()
            .await
            .map_err(|e| anyhow!("Failed to get target_identifier: {:?}", e))?;

        // Construct RepositoryTarget from same args as `ffx target repository register`
        let repo_target_info = FfxDaemonRepositoryTarget::try_from(FfxCliRepositoryTarget {
            repo_name: Some(repo_name.clone()),
            target_identifier: Some(target_identifier.clone()),
            aliases: Some(self.cmd.alias),
            storage_type: Some(FfxCliRepositoryStorageType::Ephemeral),
            ..Default::default()
        })
        .map_err(|e| anyhow!("Failed to build RepositoryTarget: {:?}", e))?;

        // Create PmRepository and RepoClient
        let repo_path = fs::canonicalize(repo_path.as_str())
            .map_err(|e| anyhow!("Failed to canonicalize repo_path: {:?}", e))?;
        let pm_backend = PmRepository::new(
            repo_path
                .clone()
                .try_into()
                .map_err(|e| anyhow!("Failed to convert PathBuf to Utf8PathBuf: {:?}", e))?,
        );
        let pm_repo_client = RepoClient::from_trusted_remote(Box::new(pm_backend) as Box<_>)
            .await
            .map_err(|e| anyhow!("Failed to create RepoClient: {:?}", e))?;

        // Add PmRepository to RepositoryManager
        let repo_manager: Arc<RepositoryManager> = RepositoryManager::new();
        repo_manager.add(repo_name.clone(), pm_repo_client);
        let repo = repo_manager
            .get(&repo_target_info.repo_name)
            .ok_or_else(|| FfxCliRepositoryError::NoMatchingRepository)
            .map_err(|e| anyhow!("Failed to fetch repository: {:?}", e))?;

        // Serve RepositoryManager over a RepositoryServer
        let (server_fut, _, server) =
            RepositoryServer::builder(repo_server_listen_addr, Arc::clone(&repo_manager))
                .start()
                .await
                .map_err(|e| anyhow!("Failed to start repository server: {:?}", e))?;

        // Write port file if needed
        if let Some(port_path) = self.cmd.port_path {
            let port = server.local_addr().port().to_string();

            fs::write(port_path, port)
                .map_err(|e| anyhow!("Failed to create port file: {:?}", e))?;
        };
        let task = fasync::Task::local(server_fut);

        register_target_with_fidl_proxies(
            self.repo_proxy,
            self.rewrite_engine_proxy,
            &repo_target_info,
            &target,
            repo_server_listen_addr,
            &repo,
            self.cmd.alias_conflict_mode.into(),
        )
        .await
        .map_err(|e| anyhow!("Failed to register repository: {:?}", e))?;

        writeln!(writer, "Serving repository '{}' to target '{target_identifier}' over address '{repo_server_listen_addr}'.", repo_path.display())
            .map_err(|e| anyhow!("Failed to write to output: {:?}", e))?;

        // Wait for the server to shut down.
        task.await;

        Ok(())
    }
}

///////////////////////////////////////////////////////////////////////////////
// tests
#[cfg(test)]
mod test {
    use {
        super::*,
        assert_matches::assert_matches,
        ffx_config::{ConfigLevel, TestEnv},
        fho::macro_deps::ffx_writer::TestBuffer,
        fidl_fuchsia_developer_ffx::{
            RepositoryRegistrationAliasConflictMode, RepositoryStorageType, SshHostAddrInfo,
            TargetAddrInfo, TargetIpPort, TargetRequest,
        },
        fidl_fuchsia_net::{IpAddress, Ipv4Address},
        fidl_fuchsia_pkg::{
            MirrorConfig, RepositoryConfig, RepositoryKeyConfig, RepositoryManagerRequest,
        },
        fidl_fuchsia_pkg_rewrite::{EditTransactionRequest, EngineRequest, RuleIteratorRequest},
        fidl_fuchsia_pkg_rewrite_ext::Rule,
        fuchsia_repo::repository::HttpRepository,
        futures::{channel::mpsc, SinkExt, StreamExt as _, TryStreamExt},
        std::{
            collections::BTreeSet,
            sync::{Arc, Mutex},
            time,
        },
        url::Url,
    };

    const REPO_NAME: &str = "some-repo";
    const REPO_IPV4_ADDR: [u8; 4] = [127, 0, 0, 1];
    const REPO_ADDR: &str = "127.0.0.1";
    const REPO_PORT: u16 = 0;
    const DEVICE_PORT: u16 = 5;
    const HOST_ADDR: &str = "1.2.3.4";
    const TARGET_NODENAME: &str = "some-target";
    const EMPTY_REPO_PATH: &str = "host_x64/test_data/ffx_plugin_serve_repo/empty-repo";

    macro_rules! rule {
        ($host_match:expr => $host_replacement:expr,
         $path_prefix_match:expr => $path_prefix_replacement:expr) => {
            Rule::new($host_match, $host_replacement, $path_prefix_match, $path_prefix_replacement)
                .unwrap()
        };
    }

    #[derive(Debug, PartialEq)]
    enum TargetEvent {
        Identity,
    }

    struct FakeTarget {
        events: Arc<Mutex<Vec<TargetEvent>>>,
    }

    impl FakeTarget {
        fn new() -> (Self, TargetProxy, mpsc::Receiver<()>) {
            let (sender, target_rx) = mpsc::channel::<()>(1);
            let events = Arc::new(Mutex::new(Vec::new()));
            let events_closure = Arc::clone(&events);

            let target_proxy: TargetProxy = fho::testing::fake_proxy(move |req| match req {
                TargetRequest::Identity { responder, .. } => {
                    let mut sender = sender.clone();
                    let device_address = TargetAddrInfo::IpPort(TargetIpPort {
                        ip: IpAddress::Ipv4(Ipv4Address { addr: [127, 0, 0, 1] }),
                        scope_id: 0,
                        port: DEVICE_PORT,
                    });
                    let nodename = Some(TARGET_NODENAME.to_string());
                    let events_closure = events_closure.clone();

                    fasync::Task::local(async move {
                        events_closure.lock().unwrap().push(TargetEvent::Identity);
                        responder
                            .send(&TargetInfo {
                                nodename,
                                addresses: Some(vec![device_address.clone()]),
                                ssh_address: Some(device_address.clone()),
                                ssh_host_address: Some(SshHostAddrInfo {
                                    address: HOST_ADDR.to_string(),
                                }),
                                ..Default::default()
                            })
                            .unwrap();
                        let _send = sender.send(()).await.unwrap();
                    })
                    .detach();
                }
                _ => panic!("unexpected request: {:?}", req),
            });
            (Self { events }, target_proxy, target_rx)
        }

        fn take_events(&self) -> Vec<TargetEvent> {
            self.events.lock().unwrap().drain(..).collect::<Vec<_>>()
        }
    }

    #[derive(Debug, PartialEq)]
    enum RepositoryManagerEvent {
        Add { repo: RepositoryConfig },
    }

    struct FakeRepositoryManager {
        events: Arc<Mutex<Vec<RepositoryManagerEvent>>>,
    }

    impl FakeRepositoryManager {
        fn new() -> (Self, RepositoryManagerProxy, mpsc::Receiver<()>) {
            let (sender, repo_rx) = mpsc::channel::<()>(1);
            let events = Arc::new(Mutex::new(Vec::new()));
            let events_closure = Arc::clone(&events);

            let repo_proxy: RepositoryManagerProxy =
                fho::testing::fake_proxy(move |req| match req {
                    RepositoryManagerRequest::Add { repo, responder } => {
                        let mut sender = sender.clone();
                        let events_closure = events_closure.clone();

                        fasync::Task::local(async move {
                            events_closure
                                .lock()
                                .unwrap()
                                .push(RepositoryManagerEvent::Add { repo });
                            responder.send(Ok(())).unwrap();
                            let _send = sender.send(()).await.unwrap();
                        })
                        .detach();
                    }
                    _ => panic!("unexpected request: {:?}", req),
                });
            (Self { events }, repo_proxy, repo_rx)
        }

        fn take_events(&self) -> Vec<RepositoryManagerEvent> {
            self.events.lock().unwrap().drain(..).collect::<Vec<_>>()
        }
    }

    #[derive(Debug, PartialEq)]
    enum RewriteEngineEvent {
        ResetAll,
        ListDynamic,
        IteratorNext,
        EditTransactionAdd { rule: Rule },
        EditTransactionCommit,
    }
    struct FakeEngine {
        events: Arc<Mutex<Vec<RewriteEngineEvent>>>,
    }

    impl FakeEngine {
        fn new() -> (Self, EngineProxy, mpsc::Receiver<()>) {
            Self::with_rules(vec![])
        }

        fn with_rules(rules: Vec<Rule>) -> (Self, EngineProxy, mpsc::Receiver<()>) {
            let (sender, rewrite_engine_rx) = mpsc::channel::<()>(1);
            let rules = Arc::new(Mutex::new(rules));
            let events = Arc::new(Mutex::new(Vec::new()));
            let events_closure = Arc::clone(&events);

            let rewrite_engine_proxy: EngineProxy =
                fho::testing::fake_proxy(move |req| match req {
                    EngineRequest::StartEditTransaction { transaction, control_handle: _ } => {
                        let mut sender = sender.clone();
                        let rules = Arc::clone(&rules);
                        let events_closure = Arc::clone(&events_closure);

                        fasync::Task::local(async move {
                            let mut stream = transaction.into_stream().unwrap();
                            while let Some(request) = stream.next().await {
                                let request = request.unwrap();
                                match request {
                                    EditTransactionRequest::ResetAll { control_handle: _ } => {
                                        events_closure
                                            .lock()
                                            .unwrap()
                                            .push(RewriteEngineEvent::ResetAll);
                                    }
                                    EditTransactionRequest::ListDynamic {
                                        iterator,
                                        control_handle: _,
                                    } => {
                                        events_closure
                                            .lock()
                                            .unwrap()
                                            .push(RewriteEngineEvent::ListDynamic);
                                        let mut stream = iterator.into_stream().unwrap();

                                        let mut rules = rules.lock().unwrap().clone().into_iter();

                                        while let Some(req) = stream.try_next().await.unwrap() {
                                            let RuleIteratorRequest::Next { responder } = req;
                                            events_closure
                                                .lock()
                                                .unwrap()
                                                .push(RewriteEngineEvent::IteratorNext);

                                            if let Some(rule) = rules.next() {
                                                responder.send(&[rule.into()]).unwrap();
                                            } else {
                                                responder.send(&[]).unwrap();
                                            }
                                        }
                                    }
                                    EditTransactionRequest::Add { rule, responder } => {
                                        events_closure.lock().unwrap().push(
                                            RewriteEngineEvent::EditTransactionAdd {
                                                rule: rule.try_into().unwrap(),
                                            },
                                        );
                                        responder.send(Ok(())).unwrap()
                                    }
                                    EditTransactionRequest::Commit { responder } => {
                                        events_closure
                                            .lock()
                                            .unwrap()
                                            .push(RewriteEngineEvent::EditTransactionCommit);
                                        let res = responder.send(Ok(())).unwrap();
                                        let _send = sender.send(()).await.unwrap();
                                        res
                                    }
                                }
                            }
                        })
                        .detach();
                    }
                    _ => panic!("unexpected request: {:?}", req),
                });
            (Self { events }, rewrite_engine_proxy, rewrite_engine_rx)
        }

        fn take_events(&self) -> Vec<RewriteEngineEvent> {
            self.events.lock().unwrap().drain(..).collect::<Vec<_>>()
        }
    }

    async fn get_test_env() -> TestEnv {
        let test_env = ffx_config::test_init().await.expect("test initialization");

        test_env
            .context
            .query(REPO_FOREGROUND_FEATURE_FLAG)
            .level(Some(ConfigLevel::User))
            .set("true".into())
            .await
            .unwrap();

        test_env
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_start_register() {
        let test_env = get_test_env().await;

        test_env
            .context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::User))
            .set(TARGET_NODENAME.into())
            .await
            .unwrap();

        let (fake_target, fake_target_proxy, mut fake_target_rx) = FakeTarget::new();
        let (fake_repo, fake_repo_proxy, mut fake_repo_rx) = FakeRepositoryManager::new();
        let (fake_engine, fake_engine_proxy, mut fake_engine_rx) = FakeEngine::new();
        let tmp_port_file = tempfile::NamedTempFile::new().unwrap();

        let serve_tool = ServeTool {
            cmd: ServeCommand {
                repository: REPO_NAME.to_string(),
                address: (REPO_IPV4_ADDR, REPO_PORT).into(),
                repo_path: Some(EMPTY_REPO_PATH.to_string()),
                alias: vec!["example.com".into(), "fuchsia.com".into()],
                storage_type: Some(RepositoryStorageType::Ephemeral),
                alias_conflict_mode: RepositoryRegistrationAliasConflictMode::Replace,
                port_path: Some(tmp_port_file.path().to_string_lossy().into()),
            },
            context: test_env.context.clone(),
            target_proxy: fake_target_proxy,
            repo_proxy: fake_repo_proxy,
            rewrite_engine_proxy: fake_engine_proxy,
        };

        let test_stdout = TestBuffer::default();
        let writer = SimpleWriter::new_buffers(test_stdout.clone(), Vec::new());

        // Run main in background
        let _task = fasync::Task::local(serve_tool.main(writer));

        // Future resolves once repo server communicates with them.
        let _timeout = timeout(time::Duration::from_secs(10), async {
            let _ = fake_target_rx.next().await.unwrap();
            let _ = fake_repo_rx.next().await.unwrap();
            let _ = fake_engine_rx.next().await.unwrap();
        })
        .await
        .unwrap();

        assert_eq!(fake_target.take_events(), vec![TargetEvent::Identity],);

        assert_eq!(
            fake_repo.take_events(),
            vec![RepositoryManagerEvent::Add {
                repo: RepositoryConfig {
                    mirrors: Some(vec![MirrorConfig {
                        mirror_url: Some(format!(
                            "http://{}:{}/{}",
                            REPO_ADDR, REPO_PORT, REPO_NAME
                        )),
                        subscribe: Some(true),
                        ..Default::default()
                    }]),
                    repo_url: Some(format!("fuchsia-pkg://{}", REPO_NAME)),
                    root_keys: Some(vec![RepositoryKeyConfig::Ed25519Key(vec![
                        29, 76, 86, 76, 184, 70, 108, 73, 249, 127, 4, 47, 95, 63, 36, 35, 101,
                        255, 212, 33, 10, 154, 26, 130, 117, 157, 125, 88, 175, 214, 109, 113,
                    ])]),
                    root_version: Some(1),
                    root_threshold: Some(1),
                    use_local_mirror: Some(false),
                    storage_type: Some(fidl_fuchsia_pkg::RepositoryStorageType::Ephemeral),
                    ..Default::default()
                }
            }],
        );

        assert_eq!(
            fake_engine.take_events(),
            vec![
                RewriteEngineEvent::ListDynamic,
                RewriteEngineEvent::IteratorNext,
                RewriteEngineEvent::ResetAll,
                RewriteEngineEvent::EditTransactionAdd {
                    rule: rule!("example.com" => REPO_NAME, "/" => "/"),
                },
                RewriteEngineEvent::EditTransactionAdd {
                    rule: rule!("fuchsia.com" => REPO_NAME, "/" => "/"),
                },
                RewriteEngineEvent::EditTransactionCommit,
            ],
        );

        // Get dynamic port
        let dynamic_repo_port =
            fs::read_to_string(tmp_port_file.path()).unwrap().parse::<u16>().unwrap();
        tmp_port_file.close().unwrap();

        // Check repository state.
        let http_repo = HttpRepository::new(
            fuchsia_hyper::new_client(),
            Url::parse(&format!("http://{REPO_ADDR}:{dynamic_repo_port}/{REPO_NAME}")).unwrap(),
            Url::parse(&format!("http://{REPO_ADDR}:{dynamic_repo_port}/{REPO_NAME}/blobs"))
                .unwrap(),
            BTreeSet::new(),
        );
        let mut repo_client = RepoClient::from_trusted_remote(http_repo).await.unwrap();

        assert_matches!(repo_client.update().await, Ok(true));
    }
}
