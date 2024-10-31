// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::environment::EnvironmentKind;
use crate::EnvironmentContext;
use anyhow::{anyhow, Result};
use camino::Utf8Path;
use std::fs::create_dir_all;
use std::path::{Path, PathBuf};

use std::env::var;

pub const ENV_FILE: &str = ".ffx_env";
pub const USER_FILE: &str = ".ffx_user_config.json";
pub const DEFAULT_BUILD_CONFIG_FILE: &str = "ffx-config.json";

impl EnvironmentContext {
    pub fn get_default_user_file_path(&self) -> Result<PathBuf> {
        match self.env_kind().isolate_root() {
            Some(isolate_root) => Ok(isolate_root.join(USER_FILE)),
            _ => get_default_user_file_path(),
        }
    }

    pub fn get_default_env_path(&self) -> Result<PathBuf> {
        match self.env_kind().isolate_root() {
            Some(isolate_root) => Ok(isolate_root.join(ENV_FILE)),
            _ => default_env_path(),
        }
    }

    pub fn get_default_build_dir_config_path(&self, build_dir: &Path) -> Result<PathBuf> {
        let filename = build_dir.join(DEFAULT_BUILD_CONFIG_FILE);
        Ok(filename)
    }

    /// If this environment context has an explicitly set build config path,
    /// return it. Otherwise None.
    pub fn get_build_config_file(&self) -> Option<&Utf8Path> {
        match self.env_kind() {
            EnvironmentKind::ConfigDomain { domain, .. } => domain.get_build_config_file(),
            _ => None,
        }
    }

    pub fn get_default_ascendd_path(&self) -> Result<PathBuf> {
        match (self.env_var("ASCENDD"), self.env_kind()) {
            (Ok(path), _) => Ok(PathBuf::from(&path)),
            (_, EnvironmentKind::InTree { build_dir: Some(p), .. }) => {
                Ok(p.join(".ffx-daemon/daemon.sock"))
            }
            (_, EnvironmentKind::Isolated { isolate_root }) => Ok(isolate_root.join("daemon.sock")),
            (_, EnvironmentKind::ConfigDomain { isolate_root: Some(isolate_root), .. }) => {
                Ok(isolate_root.join("daemon.sock").into())
            }
            (_, _) => Ok(ascendd::default_ascendd_path()),
        }
    }

    pub fn get_runtime_path(&self) -> Result<PathBuf> {
        match self.env_kind().isolate_root() {
            Some(isolate_root) => Ok(isolate_root.join("runtime")),
            _ => get_runtime_base_path(),
        }
    }

    pub fn get_cache_path(&self) -> Result<PathBuf> {
        match self.env_kind().isolate_root() {
            Some(isolate_root) => Ok(isolate_root.join("cache")),
            _ => get_cache_base_path(),
        }
    }

    pub fn get_config_path(&self) -> Result<PathBuf> {
        match self.env_kind().isolate_root() {
            Some(isolate_root) => Ok(isolate_root.join("config")),
            _ => get_config_base_path(),
        }
    }

    pub fn get_data_path(&self) -> Result<PathBuf> {
        match self.env_kind().isolate_root() {
            Some(isolate_root) => Ok(isolate_root.join("data")),
            _ => get_data_base_path(),
        }
    }

    pub fn get_shared_data_path(&self) -> Result<PathBuf> {
        get_shared_data_base_path().and_then(|base| Ok(base.join("shared")))
    }

    pub fn get_analytics_path(&self) -> Option<PathBuf> {
        if self.has_no_environment() {
            return None;
        }
        match self.env_kind().isolate_root() {
            Some(isolate_root) => Some(isolate_root.join("metrics")),
            _ => get_analytics_base_path().ok(),
        }
    }
}

fn get_runtime_base() -> Result<PathBuf> {
    if cfg!(target_os = "macos") {
        let mut home = home::home_dir().ok_or(anyhow!("cannot find home directory"))?;
        home.push("Library");
        Ok(home)
    } else {
        var("XDG_RUNTIME_HOME").map(PathBuf::from).or_else(|_| {
            let mut home = home::home_dir().ok_or(anyhow!("cannot find home directory"))?;
            home.push(".local");
            home.push("share");
            Ok(home)
        })
    }
}

fn get_runtime_base_path() -> Result<PathBuf> {
    let mut path = get_runtime_base()?;
    path.push("Fuchsia");
    path.push("ffx");
    path.push("runtime");
    create_dir_all(&path)?;
    Ok(path)
}

fn get_cache_base() -> Result<PathBuf> {
    if cfg!(target_os = "macos") {
        let mut home = home::home_dir().ok_or(anyhow!("cannot find home directory"))?;
        home.push("Library");
        home.push("Caches");
        Ok(home)
    } else {
        var("XDG_CACHE_HOME").map(PathBuf::from).or_else(|_| {
            let mut home = home::home_dir().ok_or(anyhow!("cannot find home directory"))?;
            home.push(".local");
            home.push("share");
            Ok(home)
        })
    }
}

fn get_cache_base_path() -> Result<PathBuf> {
    let mut path = get_cache_base()?;
    path.push("Fuchsia");
    path.push("ffx");
    path.push("cache");
    create_dir_all(&path)?;
    Ok(path)
}

fn get_config_base() -> Result<PathBuf> {
    if cfg!(target_os = "macos") {
        let mut home = home::home_dir().ok_or(anyhow!("cannot find home directory"))?;
        home.push("Library");
        home.push("Preferences");
        Ok(home)
    } else {
        var("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|_| {
            let mut home = home::home_dir().ok_or(anyhow!("cannot find home directory"))?;
            home.push(".local");
            home.push("share");
            Ok(home)
        })
    }
}

fn get_config_base_path() -> Result<PathBuf> {
    let mut path = get_config_base()?;
    path.push("Fuchsia");
    path.push("ffx");
    path.push("config");
    create_dir_all(&path)?;
    Ok(path)
}

fn get_analytics_base_path() -> Result<PathBuf> {
    let mut path = get_config_base()?;
    path.push("Fuchsia");
    path.push("metrics");
    create_dir_all(&path)?;
    Ok(path)
}

fn default_env_path() -> Result<PathBuf> {
    // Environment file that keeps track of configuration files
    get_config_base_path().map(|mut path| {
        path.push(ENV_FILE);
        path
    })
}

fn get_data_base() -> Result<PathBuf> {
    if cfg!(target_os = "macos") {
        let mut home = home::home_dir().ok_or(anyhow!("cannot find home directory"))?;
        home.push("Library");
        Ok(home)
    } else {
        var("XDG_DATA_HOME").map(PathBuf::from).or_else(|_| {
            let mut home = home::home_dir().ok_or(anyhow!("cannot find home directory"))?;
            home.push(".local");
            home.push("share");
            Ok(home)
        })
    }
}

fn get_state_base() -> Result<PathBuf> {
    if cfg!(target_os = "macos") {
        let mut home = home::home_dir().ok_or(anyhow!("cannot find home directory"))?;
        home.push("Library");
        Ok(home)
    } else {
        var("XDG_STATE_HOME").map(PathBuf::from).or_else(|_| {
            let mut home = home::home_dir().ok_or(anyhow!("cannot find home directory"))?;
            home.push(".local");
            home.push("share");
            Ok(home)
        })
    }
}

fn get_data_base_path() -> Result<PathBuf> {
    let mut path = get_data_base()?;
    path.push("Fuchsia");
    path.push("ffx");
    create_dir_all(&path)?;
    Ok(path)
}

fn get_shared_data_base_path() -> Result<PathBuf> {
    let mut path = get_state_base()?;
    path.push("Fuchsia");
    path.push("ffx");
    create_dir_all(&path)?;
    Ok(path)
}

fn get_default_user_file_path() -> Result<PathBuf> {
    // Default user configuration file
    const DEFAULT_USER_CONFIG: &str = ".ffx_user_config.json";

    let mut default_path = get_config_base_path()?;
    default_path.push(DEFAULT_USER_CONFIG);
    Ok(default_path)
}
