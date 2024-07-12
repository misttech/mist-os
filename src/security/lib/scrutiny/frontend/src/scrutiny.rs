// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::shell::Shell;
use anyhow::Result;
use scrutiny::engine::dispatcher::ControllerDispatcher;
use scrutiny::engine::hook::PluginHooks;
use scrutiny::model::model::DataModel;
use scrutiny_config::{Config, LoggingVerbosity};
use simplelog::{Config as SimpleLogConfig, LevelFilter, WriteLogger};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::sync::{Arc, RwLock};

/// Holds a reference to core objects required by the application to run.
pub struct Scrutiny {
    #[allow(dead_code)]
    plugin: PluginHooks,
    #[allow(dead_code)]
    dispatcher: Arc<RwLock<ControllerDispatcher>>,
    shell: Shell,
    config: Config,
}

impl Scrutiny {
    /// Creates the DataModel, ControllerDispatcher, CollectorScheduler and
    /// PluginManager.
    pub fn new(config: Config, plugin: PluginHooks) -> Result<Self> {
        let log_level = match config.runtime.logging.verbosity {
            LoggingVerbosity::Error => LevelFilter::Error,
            LoggingVerbosity::Warn => LevelFilter::Warn,
            LoggingVerbosity::Info => LevelFilter::Info,
            LoggingVerbosity::Debug => LevelFilter::Debug,
            LoggingVerbosity::Trace => LevelFilter::Trace,
            LoggingVerbosity::Off => LevelFilter::Off,
        };

        if let Ok(log_file) = File::create(config.runtime.logging.path.clone()) {
            let _ = WriteLogger::init(log_level, SimpleLogConfig::default(), log_file);
        }

        // Collect all the data into the model.
        let model = Arc::new(DataModel::new(config.runtime.model.clone())?);
        if let Some(collector) = &plugin.collector {
            collector.collect(model.clone())?;
        }

        // Register all the functions.
        let dispatcher = Arc::new(RwLock::new(ControllerDispatcher::new(Arc::clone(&model))));
        for (namespace, controller) in plugin.controllers.iter() {
            let mut dispatcher = dispatcher.write().unwrap();
            dispatcher.add(namespace.clone(), Arc::clone(&controller)).unwrap();
        }

        let shell = Shell::new(Arc::clone(&dispatcher), config.runtime.logging.silent_mode);
        Ok(Self { plugin, dispatcher, shell, config })
    }

    /// Schedules the DataCollectors to run and starts the REST service.
    pub fn run(&mut self) -> Result<String> {
        if let Some(command) = &self.config.launch.command {
            return self.shell.execute(command.to_string());
        } else if let Some(script) = &self.config.launch.script_path {
            let script_file = BufReader::new(File::open(script)?);
            let mut script_output = String::new();
            for line in script_file.lines() {
                script_output.push_str(&self.shell.execute(line?)?);
            }
            return Ok(script_output);
        }
        Ok(String::new())
    }
}
