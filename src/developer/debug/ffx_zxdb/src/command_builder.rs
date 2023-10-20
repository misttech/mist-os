// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{ffi::OsString, path::PathBuf, process::Command};

use crate::debug_agent::DebugAgentSocket;

/// Builds command lines for `zxdb`.
pub struct CommandBuilder {
    zxdb_path: PathBuf,
    args: Vec<OsString>,
}

impl CommandBuilder {
    pub fn new(zxdb_path: PathBuf) -> Self {
        Self { zxdb_path, args: vec![] }
    }

    pub fn build(self) -> Command {
        let mut command = Command::new(self.zxdb_path);
        command.args(&self.args);
        command
    }

    pub fn into_args(self) -> Vec<OsString> {
        self.args
    }

    pub fn connect(&mut self, socket: &DebugAgentSocket) {
        self.args.push(OsString::from("--unix-connect"));
        self.args.push(socket.unix_socket_path().into());
    }

    pub fn push_str(&mut self, arg: &str) {
        self.args.push(OsString::from(arg));
    }

    pub fn extend(&mut self, args: &Vec<String>) {
        self.args.extend(args.iter().map(|s| s.into()));
    }

    pub fn attach(&mut self, attach: &String) {
        self.args.push(OsString::from("--attach"));
        self.args.push(attach.into());
    }

    pub fn attach_each(&mut self, attach: &Vec<String>) {
        for attach in attach.iter() {
            self.attach(attach);
        }
    }

    pub fn execute(&mut self, execute: &String) {
        self.args.push(OsString::from("--execute"));
        self.args.push(execute.into());
    }

    pub fn execute_each(&mut self, execute: &Vec<String>) {
        for execute in execute.iter() {
            self.execute(execute);
        }
    }
}
