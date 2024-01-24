// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Result};

use std::{
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
};

use crate::api::{Api, Component, ComponentId, CreateIssue, IssueId, UpdateIssue};

pub struct Bugspec {
    path: PathBuf,
}

impl Bugspec {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl Api for Bugspec {
    fn create_issue(&mut self, request: CreateIssue) -> Result<IssueId> {
        let mut child = Command::new(&self.path).arg("create").stdin(Stdio::piped()).spawn()?;

        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(request.to_bugspec().as_bytes())?;

        let output = child.wait_with_output()?;

        Ok(IssueId::new(core::str::from_utf8(&output.stdout)?.parse()?))
    }

    fn update_issue(&mut self, request: UpdateIssue) -> Result<()> {
        let mut child = Command::new(&self.path)
            .arg("edit")
            .arg(&format!("{}", request.id))
            .stdin(Stdio::piped())
            .spawn()?;

        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(request.to_bugspec().as_bytes())?;

        child.wait()?;

        Ok(())
    }

    fn list_components(&mut self) -> Result<Vec<Component>> {
        let output = Command::new(&self.path).arg("list-components").output()?;

        let text = String::from_utf8(output.stdout)?;

        let mut results = Vec::new();
        for line in text.lines() {
            let (id, path) =
                line.split_once(' ').context("expected component to have id and path")?;
            results.push(Component {
                id: ComponentId::new(id.parse()?),
                path: path.split(" > ").skip(2).map(str::to_string).collect(),
            });
        }

        Ok(results)
    }
}
