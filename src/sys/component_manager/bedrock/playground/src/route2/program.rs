// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use replace_with::replace_with;
use sandbox::{Capability, Dict, Routable, Router};
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
        output: Dict,
    },
}

impl Program {
    pub fn new(name: String, runner: Runner) -> Self {
        Program(Arc::new(Inner { name, state: Mutex::new(ProgramState::MissingInput), runner }))
    }

    pub fn output(&self) -> Router {
        Router::from_routable(Arc::downgrade(&self.0))
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
    pub fn start(&self) -> Dict {
        let mut state = self.state.lock().unwrap();
        replace_with(state.deref_mut(), |state| state.start(&self.name, self.runner.clone()));
        match state.deref() {
            ProgramState::MissingInput | ProgramState::NotRunning { .. } => {
                unreachable!("just started the program")
            }
            ProgramState::Running { output } => output.try_clone().unwrap(),
        }
    }

    pub fn set_input(&self, input: Dict) {
        let mut state = self.state.lock().unwrap();
        replace_with(state.deref_mut(), move |state| state.set_input(input));
    }
}

impl Routable for Inner {
    fn route(&self, request: sandbox::Request, completer: sandbox::Completer) {
        // Start the program if not started, then forward the request to its output.
        let router = self.start();
        router.route(request, completer);
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

#[derive(Clone)]
pub struct Runner(pub Arc<dyn Fn(&str, Dict) -> Dict + Send + Sync>);

impl fmt::Debug for Runner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Runner").finish()
    }
}
