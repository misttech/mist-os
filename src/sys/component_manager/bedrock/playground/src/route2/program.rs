// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::anyhow;
use replace_with::replace_with;
use sandbox::{AsRouter, Dict, Router};
use std::{
    fmt,
    ops::{Deref, DerefMut},
    sync::{Arc, Mutex},
};

/// A program consumes a dictionary and provides an router object which let others obtain
/// capabilities from it.
pub struct Program(Arc<Inner>);

pub struct Inner {
    name: String,
    state: Mutex<ProgramState>,
    runner: Runner,
}

enum ProgramState {
    /// Temporary state until the input dict is populated.
    MissingInput,
    NotRunning {
        input: Dict,
    },
    Running {
        output: Router,
    },
}

impl Program {
    pub fn new(name: String, runner: Runner) -> Self {
        Program(Arc::new(Inner { name, state: Mutex::new(ProgramState::MissingInput), runner }))
    }
}

impl Deref for Program {
    type Target = Inner;

    fn deref(&self) -> &Self::Target {
        self.0.deref()
    }
}

impl Inner {
    /// Explicitly start the program.
    pub fn start(&self) -> Router {
        let mut state = self.state.lock().unwrap();
        replace_with(state.deref_mut(), |state| state.start(&self.name, self.runner.clone()));
        match state.deref_mut() {
            ProgramState::MissingInput | ProgramState::NotRunning { .. } => {
                unreachable!("just started the program")
            }
            ProgramState::Running { output } => output.clone(),
        }
    }

    pub fn set_input(&self, input: Dict) {
        let mut state = self.state.lock().unwrap();
        replace_with(state.deref_mut(), move |state| state.set_input(input));
    }
}

impl ProgramState {
    fn set_input(self, input: Dict) -> ProgramState {
        match self {
            ProgramState::MissingInput => ProgramState::NotRunning { input },
            ProgramState::NotRunning { .. } => unimplemented!(),
            ProgramState::Running { .. } => unimplemented!(),
        }
    }

    fn start(self, name: &str, runner: Runner) -> ProgramState {
        match self {
            ProgramState::MissingInput => panic!("must set input dict before start"),
            ProgramState::NotRunning { input } => {
                ProgramState::Running { output: (runner.0)(name, input) }
            }
            ProgramState::Running { output } => ProgramState::Running { output },
        }
    }
}

impl AsRouter for Program {
    fn as_router(&self) -> Router {
        let weak = Arc::downgrade(&self.0);
        let route_fn = move |request, completer| {
            match weak.upgrade() {
                Some(program) => {
                    // Start the program if not started, then forward the request to its output.
                    let router = program.start();
                    router.route(request, completer);
                }
                None => completer.complete(Err(anyhow!("program is destroyed"))),
            }
        };
        Router::new(route_fn)
    }
}

#[derive(Clone)]
pub struct Runner(pub Arc<dyn Fn(&str, Dict) -> Router + Send + Sync>);

impl fmt::Debug for Runner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Runner").finish()
    }
}
