// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{DirectConnector, TryFromEnv};
use async_lock::RwLock;
use ffx_command::FfxCommandLine;
use ffx_command_error::{return_bug, Result};
use ffx_config::EnvironmentContext;
use ffx_core::Injector;
use futures::future::LocalBoxFuture;
use std::fmt;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

/// This is the trait that defines the information for a given target device.
pub trait FhoTargetInfo {
    /// The nodename of the device, if known.
    fn nodename(&self) -> Option<String>;

    /// The serial number of the device, if known.
    fn serial_number(&self) -> Option<String>;

    /// The TCP/IP addresses of the device, if known. Note: This is a SocketAddr so
    /// the scope_id of any IPv6 address is passed along. The port number is always
    /// zero and is meaningless.
    fn addresses(&self) -> Vec<std::net::SocketAddr>;

    /// The address and port for the ssh service on the device.
    fn ssh_address(&self) -> Option<std::net::SocketAddr>;
}

// This trait can a.) probably use more members, and b.) be something that is made public inside of
// the `target` library.
#[mockall::automock]
pub trait DeviceLookup {
    fn target_spec(&self, env: EnvironmentContext) -> LocalBoxFuture<'_, Result<Option<String>>>;

    fn resolve_target_query_to_info(
        &self,
        query: Option<String>,
        env: EnvironmentContext,
    ) -> LocalBoxFuture<'_, Result<Vec<Box<dyn FhoTargetInfo>>>>;
}

#[derive(Clone)]
pub enum FhoConnectionBehavior {
    DaemonConnector(Arc<dyn Injector>),
    DirectConnector(Arc<dyn DirectConnector>),
}

// Manually implement Debug here so we can skip implementing
// Debug on the traits of the variant data.
impl fmt::Debug for FhoConnectionBehavior {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::DaemonConnector(_) => "DaemonConnector",
            Self::DirectConnector(_) => "DirectConnector",
        };
        write!(f, "{name}")
    }
}

#[derive(Clone)]
pub struct FhoEnvironment {
    ffx: FfxCommandLine,
    context: EnvironmentContext,
    /// Defines how to connect to a Fuchsia device. It can be
    /// lazily initialized, and potentially multiple threads,
    /// so using Arc<RwLock<>> container.
    behavior: Arc<RwLock<Option<FhoConnectionBehavior>>>,
    lookup: Arc<RwLock<Rc<Option<Box<dyn DeviceLookup>>>>>,
}

impl FhoEnvironment {
    pub fn new(context: &EnvironmentContext, ffx: &FfxCommandLine) -> Self {
        tracing::info!("FhoEnvironment created");
        FhoEnvironment {
            behavior: Arc::new(RwLock::new(None)),
            ffx: ffx.clone(),
            context: context.clone(),
            lookup: Arc::new(RwLock::new(None.into())),
        }
    }

    /// Create new instance for use in tests.
    pub fn new_with_args(context: &EnvironmentContext, argv: &[impl AsRef<str>]) -> Self {
        tracing::info!("FhoEnvironment test instance with args created");
        FhoEnvironment {
            behavior: Arc::new(RwLock::new(None)),
            ffx: FfxCommandLine::new(None, argv).unwrap(),
            context: context.clone(),
            lookup: Arc::new(RwLock::new(None.into())),
        }
    }

    pub fn new_for_test<T: DeviceLookup + 'static>(
        context: &EnvironmentContext,
        ffx: &FfxCommandLine,
        behavior: FhoConnectionBehavior,
        lookup: Option<T>,
    ) -> Self {
        let boxed: Option<Box<dyn DeviceLookup>> =
            if let Some(l) = lookup { Some(Box::new(l)) } else { None };
        FhoEnvironment {
            behavior: Arc::new(RwLock::new(Some(behavior))),
            ffx: ffx.clone(),
            context: context.clone(),
            lookup: Arc::new(RwLock::new(Rc::new(boxed))),
        }
    }
    /// This attempts to wrap errors around a potential failure in the underlying connection being
    /// used to facilitate FIDL protocols. This should NOT be used by developers, this is intended
    /// to be used outside of the scope of an ffx subtool (outside of the `main` function).
    pub async fn maybe_wrap_connection_errors<T>(&self, res: Result<T>) -> Result<T> {
        if let Some(behavior) = self.behavior().await {
            match (res, behavior) {
                (Err(e), FhoConnectionBehavior::DirectConnector(ref dc)) => {
                    return Err(dc.wrap_connection_errors(e).await);
                }
                (r, _) => r,
            }
        } else {
            res
        }
    }

    pub fn ffx_command(&self) -> &FfxCommandLine {
        &self.ffx
    }

    pub fn environment_context(&self) -> &EnvironmentContext {
        &self.context
    }

    pub async fn behavior(&self) -> Option<FhoConnectionBehavior> {
        if let Some(ref b) = *self.behavior.read().await {
            Some(b.clone())
        } else {
            None
        }
    }

    /// Fuchsia device lookup API.
    pub async fn lookup(&self) -> Rc<Option<Box<dyn DeviceLookup>>> {
        let ref value = *self.lookup.read().await;
        value.clone()
    }

    pub async fn set_behavior(&self, new_behavior: FhoConnectionBehavior) {
        tracing::debug!("setting behavior");
        let mut behavior = self.behavior.write().await;
        *behavior = Some(new_behavior);
        tracing::debug!("setting behavior done");
    }
    pub async fn set_lookup(&self, new_lookup: Box<dyn DeviceLookup>) {
        tracing::debug!("setting lookup");
        let mut lookup = self.lookup.write().await;
        *lookup = Rc::new(Some(new_lookup));
        tracing::debug!("setting lookup done");
    }

    /// While the surface of this function is a little awkward, this is necessary to provide a
    /// readable error. Authors shouldn't use this directly, they should instead use
    /// `TryFromEnv`.
    pub async fn injector<T: TryFromEnv>(&self) -> Result<Arc<dyn Injector>> {
        let strict = self.ffx.global.strict;
        if let Some(behavior) = self.behavior().await {
            match behavior {
                FhoConnectionBehavior::DaemonConnector(ref dc) => Ok(dc.clone()),
                _ => {
                    if strict {
                        Err(
                        ffx_command::user_error!(
                            "ffx-strict doesn't support use of the daemon, which is used to allocate '{}'. This command must either be re-written or you should not use it.",
                            std::any::type_name::<T>()
                        )
                    )
                    } else {
                        Err(ffx_command::user_error!(
                        "Attempting to use the daemon to allocate '{}', which is not yet supported with {:?}",
                        std::any::type_name::<T>(), behavior
                    ))
                    }
                }
            }
        } else {
            return_bug!("Connection behavior is not initialized")
        }
    }

    /// Update the log file name which can be influenced by the
    /// FfxMain implementation being run.
    pub fn update_log_file(&self, basename: Option<String>) -> Result<()> {
        if let Some(basename) = basename {
            // If the base name is the default, no action is needed.
            if basename == ffx_config::logging::LOG_BASENAME {
                return Ok(());
            }
            // If the log was specified on the command line, no action is needed
            if self.ffx.global.log_destination.is_some() {
                return Ok(());
            }

            // Some simple validation of the basename.
            if basename.is_empty() {
                return_bug!("basename cannot be empty")
            }

            // Build the path to the new log file.
            let dir: PathBuf =
                self.context.get(ffx_config::logging::LOG_DIR).unwrap_or_else(|_| ".".into());
            let mut log_file = dir.join(basename);
            log_file.set_extension("log");

            tracing::info!("Switching log file to {log_file:?}");
            eprintln!("Switching log file to {log_file:?}");

            ffx_config::logging::change_log_file(&log_file)?;
        }

        Ok(())
    }
}
