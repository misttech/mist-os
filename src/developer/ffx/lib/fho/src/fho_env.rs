// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{DirectConnector, TryFromEnv};
use async_lock::RwLock;
use ffx_command::FfxCommandLine;
use ffx_command_error::{return_bug, Result};
use ffx_config::EnvironmentContext;
use ffx_core::Injector;
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub enum FhoConnectionBehavior {
    DaemonConnector(Arc<dyn Injector>),
    DirectConnector(Arc<dyn DirectConnector>),
}

// Interfaces can provide additional errors
pub trait EnvironmentInterface: Any {
    fn wrap_main_errors(&self, err: crate::Error) -> crate::Error;
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

// Type magic allowing us to both cast to Any, as well as to get back to this interface
impl dyn EnvironmentInterface {
    fn as_any(&self) -> &dyn Any {
        self
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
    // Store information relevant to dependent crates, at most one for each type.
    interfaces: Arc<Mutex<HashMap<TypeId, Box<dyn EnvironmentInterface>>>>,
}

impl FhoEnvironment {
    pub fn new(context: &EnvironmentContext, ffx: &FfxCommandLine) -> Self {
        tracing::info!("FhoEnvironment created");
        FhoEnvironment {
            behavior: Arc::new(RwLock::new(None)),
            ffx: ffx.clone(),
            context: context.clone(),
            interfaces: Default::default(),
        }
    }

    /// Create new instance for use in tests.
    pub fn new_with_args(context: &EnvironmentContext, argv: &[impl AsRef<str>]) -> Self {
        tracing::info!("FhoEnvironment test instance with args created");
        FhoEnvironment {
            behavior: Arc::new(RwLock::new(None)),
            ffx: FfxCommandLine::new(None, argv).unwrap(),
            context: context.clone(),
            interfaces: Default::default(),
        }
    }

    /// This attempts to wrap errors around a potential failure in the underlying connection being
    /// used to facilitate FIDL protocols. This should NOT be used by developers, this is intended
    /// to be used outside of the scope of an ffx subtool (outside of the `main` function).
    pub async fn maybe_wrap_connection_errors<T>(&self, res: Result<T>) -> Result<T> {
        if let Some(behavior) = self.behavior().await {
            match (res, behavior) {
                (Err(e), FhoConnectionBehavior::DirectConnector(ref dc)) => {
                    return Err(dc.wrap_connection_errors(e));
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
            tracing::debug!("Using {b:?} behavior");
            Some(b.clone())
        } else {
            None
        }
    }

    pub async fn set_behavior(&self, new_behavior: FhoConnectionBehavior) {
        tracing::debug!("setting behavior");
        let mut behavior = self.behavior.write().await;
        *behavior = Some(new_behavior);
        tracing::debug!("setting behavior done");
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

    pub fn get_interface<T: EnvironmentInterface + Clone + 'static>(&self) -> Option<T> {
        let interfaces = self.interfaces.lock().expect("poisoned interface map");
        let Some(ei) = interfaces.get(&TypeId::of::<T>()) else {
            return None;
        };
        ei.as_any().downcast_ref::<T>().map(|t| t.clone())
    }
    pub fn set_interface<T: EnvironmentInterface + 'static>(&self, ei: T) {
        let mut interfaces = self.interfaces.lock().expect("poisoned interface map");
        interfaces.insert(TypeId::of::<T>(), Box::new(ei));
    }

    pub fn wrap_main_result<T>(&self, res: Result<T>) -> Result<T> {
        match res {
            Ok(_) => res,
            Err(mut e) => {
                let interfaces = self.interfaces.lock().expect("poisoned interface map");
                for i in interfaces.values() {
                    e = i.wrap_main_errors(e);
                }
                Err(e)
            }
        }
    }
}
