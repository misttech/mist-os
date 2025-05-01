// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub use crate::fho::connector::DirectConnector;
use ffx_command_error::{return_bug, Result};
use ffx_core::Injector;
use fho::TryFromEnv;
use std::fmt;
use std::sync::{Arc, Mutex};

pub mod connector;

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

#[derive(Clone, Default)]
pub struct FhoTargetEnvironment {
    /// Defines how to connect to a Fuchsia device. It can be
    /// lazily initialized, and potentially used by multiple threads,
    /// hence the complicated type.
    behavior: Arc<Mutex<Option<Box<FhoConnectionBehavior>>>>,
}

impl FhoTargetEnvironment {
    pub fn new_for_test(behavior: FhoConnectionBehavior) -> Self {
        FhoTargetEnvironment { behavior: Arc::new(Mutex::new(Some(Box::new(behavior)))) }
    }

    /// This attempts to wrap errors around a potential failure in the underlying connection being
    /// used to facilitate FIDL protocols. This should NOT be used by developers, this is intended
    /// to be used outside of the scope of an ffx subtool (outside of the `main` function).
    fn maybe_wrap_connection_errors(&self, err: fho::Error) -> fho::Error {
        if let Some(behavior) = self.behavior() {
            if let FhoConnectionBehavior::DirectConnector(ref dc) = behavior {
                return dc.wrap_connection_errors(err);
            }
        }
        err
    }

    pub fn behavior(&self) -> Option<FhoConnectionBehavior> {
        let b = self.behavior.lock().expect("poisoned behavior lock");
        b.as_ref().map(|boxed| *boxed.clone())
    }

    pub fn set_behavior(&self, new_behavior: FhoConnectionBehavior) {
        tracing::debug!("setting behavior");
        let mut behavior = self.behavior.lock().expect("poisoned behavior lock");
        *behavior = Some(Box::new(new_behavior));
        tracing::debug!("setting behavior done");
    }
    /// While the surface of this function is a little awkward, this is necessary to provide a
    /// readable error. Authors shouldn't use this directly, they should instead use
    /// `TryFromEnv`.
    pub async fn injector<T: TryFromEnv>(
        &self,
        env: &fho::FhoEnvironment,
    ) -> Result<Arc<dyn Injector>> {
        let strict = env.ffx_command().global.strict;
        if let Some(behavior) = self.behavior() {
            match behavior {
                FhoConnectionBehavior::DaemonConnector(ref dc) => Ok(dc.clone()),
                _ => {
                    if strict {
                        Err(
                        ffx_command_error::user_error!(
                            "ffx-strict doesn't support use of the daemon, which is used to allocate '{}'. This command must either be re-written or you should not use it.",
                            std::any::type_name::<T>()
                        )
                    )
                    } else {
                        Err(ffx_command_error::user_error!(
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
}

impl fho::EnvironmentInterface for FhoTargetEnvironment {
    fn wrap_main_errors(&self, err: fho::Error) -> fho::Error {
        self.maybe_wrap_connection_errors(err)
    }
}
pub fn target_interface(env: &fho::FhoEnvironment) -> FhoTargetEnvironment {
    if env.get_interface::<FhoTargetEnvironment>().is_none() {
        let target_interface = FhoTargetEnvironment::default();
        env.set_interface(target_interface);
    }
    env.get_interface::<FhoTargetEnvironment>().expect("No target interface in FhoEnvironment??")
}
