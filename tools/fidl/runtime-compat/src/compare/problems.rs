// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use std::fmt::Display;

use super::Path;

#[derive(PartialEq, PartialOrd, Eq, Ord, Debug, Clone, Copy)]
pub enum CompatibilityDegree {
    Incompatible,
    WeaklyCompatible,
    StronglyCompatible,
}

#[derive(PartialEq, Eq, Debug, Ord, PartialOrd)]
enum ProblemScopeType {
    Platform,
    Protocol,
    Type,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum CompatibilityScope {
    Platform { external: Path, platform: Path },
    Protocol { client: Path, server: Path },
    Type { sender: Path, receiver: Path },
}

impl CompatibilityScope {
    fn scope_type(&self) -> ProblemScopeType {
        use ProblemScopeType::*;
        match self {
            CompatibilityScope::Platform { external: _, platform: _ } => Platform,
            CompatibilityScope::Protocol { client: _, server: _ } => Protocol,
            CompatibilityScope::Type { sender: _, receiver: _ } => Type,
        }
    }
    fn paths(&self) -> (&Path, &Path) {
        match self {
            CompatibilityScope::Platform { external, platform } => (external, platform),
            CompatibilityScope::Protocol { client, server } => (client, server),
            CompatibilityScope::Type { sender, receiver } => (sender, receiver),
        }
    }
    fn path(&self) -> String {
        let (a, b) = self.paths();
        let (a, b) = (a.string(), b.string());
        if a == b {
            a
        } else {
            if a.is_empty() {
                b
            } else if b.is_empty() {
                a
            } else if a.starts_with(&b) {
                a
            } else if b.starts_with(&a) {
                b
            } else {
                todo!("Work out how to report different paths where neither is a prefix");
            }
        }
    }
    fn levels(&self) -> (&str, &str) {
        match self {
            CompatibilityScope::Platform { external, platform } => {
                (external.api_level(), platform.api_level())
            }
            CompatibilityScope::Protocol { client, server } => {
                (client.api_level(), server.api_level())
            }
            CompatibilityScope::Type { sender, receiver } => {
                (sender.api_level(), receiver.api_level())
            }
        }
    }
}

impl Display for CompatibilityScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.path())
    }
}

impl PartialOrd for CompatibilityScope {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.path().partial_cmp(&other.path())
    }
}

impl Ord for CompatibilityScope {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match self.partial_cmp(other) {
            Some(ord) => ord,
            None => self.scope_type().cmp(&other.scope_type()),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct CompatibilityProblem {
    scope: CompatibilityScope,
    warning: bool,
    message: String,
}

impl Display for CompatibilityProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.warning {
            writeln!(f, "WRN: {}", self.message)?;
        } else {
            writeln!(f, "ERR: {}", self.message)?;
        }
        writeln!(f, " at: {}", self.scope)
    }
}

impl std::fmt::Debug for CompatibilityProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CompatibilityProblem::")?;
        match self.scope.scope_type() {
            ProblemScopeType::Platform => {
                write!(f, "platform")?;
                assert!(!self.warning);
            }
            ProblemScopeType::Protocol => {
                write!(f, "protocol")?;
                assert!(!self.warning);
            }
            ProblemScopeType::Type => {
                if self.warning {
                    write!(f, "type_warning")?;
                } else {
                    write!(f, "type_error")?;
                }
            }
        }
        let levels = self.scope.levels();
        write!(f, "({:?}, {:?})", levels.0, levels.1)?;
        write!(f, "{{ path={:?}, message={:?} }}", self.scope.path(), self.message)?;
        Ok(())
    }
}

impl PartialOrd for CompatibilityProblem {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        match self.warning.partial_cmp(&other.warning) {
            Some(core::cmp::Ordering::Equal) => {}
            ord => return ord,
        }
        match self.scope.partial_cmp(&other.scope) {
            Some(core::cmp::Ordering::Equal) => {}
            ord => return ord,
        }
        None
    }
}

impl Ord for CompatibilityProblem {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.warning.cmp(&other.warning)
        // match self.warning.cmp(&other.warning) {
        //     std::cmp::Ordering::Equal => self.scope.cmp(&other.scope),
        //     other => other,
        // }
    }
}

#[test]
fn test_compatibility_problem_comparison() {
    let mk_path = || Path::new(&flyweights::FlyStr::new(""));
    let warning = CompatibilityProblem {
        scope: CompatibilityScope::Platform { external: mk_path(), platform: mk_path() },
        warning: true,
        message: "beware".to_owned(),
    };
    let error = CompatibilityProblem {
        scope: CompatibilityScope::Platform { external: mk_path(), platform: mk_path() },
        warning: false,
        message: "to err is human".to_owned(),
    };

    assert!(error < warning);
}

#[derive(Default, Debug)]
pub struct CompatibilityProblems(Vec<CompatibilityProblem>);

impl CompatibilityProblems {
    pub fn platform(&mut self, external: &Path, platform: &Path, message: impl AsRef<str>) {
        self.0.push(CompatibilityProblem {
            scope: CompatibilityScope::Platform {
                external: external.clone(),
                platform: platform.clone(),
            },
            warning: false,
            message: message.as_ref().to_owned(),
        });
    }
    pub fn protocol(&mut self, client: &Path, server: &Path, message: impl AsRef<str>) {
        self.0.push(CompatibilityProblem {
            scope: CompatibilityScope::Protocol { client: client.clone(), server: server.clone() },
            warning: false,
            message: message.as_ref().to_owned(),
        });
    }
    pub fn type_error(&mut self, sender: &Path, receiver: &Path, message: impl AsRef<str>) {
        self.0.push(CompatibilityProblem {
            scope: CompatibilityScope::Type { sender: sender.clone(), receiver: receiver.clone() },
            warning: false,
            message: message.as_ref().to_owned(),
        });
    }
    pub fn type_warning(&mut self, sender: &Path, receiver: &Path, message: impl AsRef<str>) {
        self.0.push(CompatibilityProblem {
            scope: CompatibilityScope::Type { sender: sender.clone(), receiver: receiver.clone() },
            warning: true,
            message: message.as_ref().to_owned(),
        });
    }

    pub fn append(&mut self, mut other: CompatibilityProblems) {
        self.0.append(&mut other.0);
    }

    pub fn compatibility_degree(&self) -> CompatibilityDegree {
        use CompatibilityDegree::*;
        match self.0.iter().map(|p| if p.warning { WeaklyCompatible } else { Incompatible }).min() {
            Some(degree) => degree,
            None => StronglyCompatible,
        }
    }

    pub fn is_incompatible(&self) -> bool {
        self.compatibility_degree() == CompatibilityDegree::Incompatible
    }

    #[cfg(test)]
    pub fn is_compatible(&self) -> bool {
        self.0.is_empty()
    }

    #[cfg(test)]
    /// Returns true if for every problem there's a exactly one pattern that matches and vice versa.
    pub fn has_problems(&self, patterns: Vec<ProblemPattern<'_>>) -> bool {
        let matching_problems: Vec<Vec<usize>> = patterns
            .iter()
            .map(|pattern| {
                self.0
                    .iter()
                    .enumerate()
                    .filter_map(
                        |(i, problem)| {
                            if pattern.matches(problem) {
                                Some(i)
                            } else {
                                None
                            }
                        },
                    )
                    .collect()
            })
            .collect();
        let matched_problems: std::collections::BTreeSet<usize> =
            matching_problems.iter().flat_map(|ps| ps.iter().cloned()).collect();

        let mut ok = true;

        for (pattern, matching) in patterns.iter().zip(matching_problems) {
            match matching.len() {
                1 => (),
                0 => {
                    println!("Pattern doesn't match any problems: {:?}", pattern);
                    ok = false;
                }
                _ => {
                    println!("Pattern matches {} problems: {:?}", matching.len(), pattern);
                    for i in matching {
                        println!("  {:?}", self.0[i]);
                    }
                    ok = false;
                }
            }
        }
        for (i, problem) in self.0.iter().enumerate() {
            if !matched_problems.contains(&i) {
                println!("Unexpected problem: {:?}", problem);
                ok = false;
            }
        }

        ok
    }

    pub fn sort(&mut self) {
        self.0.sort()
    }
}

impl IntoIterator for CompatibilityProblems {
    type Item = CompatibilityProblem;

    type IntoIter = std::vec::IntoIter<Self::Item>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

#[cfg(test)]
#[derive(Debug)]
pub enum StringPattern<'a> {
    Equals(&'a str),
    Begins(&'a str),
    Ends(&'a str),
    Contains(&'a str),
}

#[cfg(test)]
impl StringPattern<'_> {
    pub fn matches(&self, string: impl AsRef<str>) -> bool {
        let string = string.as_ref();
        match self {
            StringPattern::Equals(pattern) => &string == pattern,
            StringPattern::Begins(pattern) => string.starts_with(pattern),
            StringPattern::Ends(pattern) => string.ends_with(pattern),
            StringPattern::Contains(pattern) => string.contains(pattern),
        }
    }
}

#[cfg(test)]
impl<'a> Into<StringPattern<'a>> for &'a str {
    fn into(self) -> StringPattern<'a> {
        StringPattern::Equals(self)
    }
}

#[cfg(test)]
#[derive(Default)]
pub struct ProblemPattern<'a> {
    warning: Option<bool>,
    message: Option<StringPattern<'a>>,
    scope: Option<ProblemScopeType>,
    path: Option<StringPattern<'a>>,
    levels: Option<(&'a str, &'a str)>,
}

#[cfg(test)]
#[allow(unused)]
impl<'a> ProblemPattern<'a> {
    pub fn matches(&self, problem: &CompatibilityProblem) -> bool {
        if let Some(warning) = self.warning {
            if warning != problem.warning {
                return false;
            }
        }
        if let Some(message) = &self.message {
            if !message.matches(&problem.message) {
                return false;
            }
        }
        if let Some(scope) = &self.scope {
            if scope != &problem.scope.scope_type() {
                return false;
            }
        }
        if let Some(path) = &self.path {
            if !path.matches(problem.scope.path()) {
                return false;
            }
        }
        if let Some(levels) = self.levels {
            if levels != problem.scope.levels() {
                return false;
            }
        }
        true
    }
    pub fn platform() -> Self {
        Self { warning: Some(false), scope: Some(ProblemScopeType::Platform), ..Default::default() }
    }
    pub fn protocol(client: &'a str, server: &'a str) -> Self {
        Self {
            warning: Some(false),
            scope: Some(ProblemScopeType::Protocol),
            levels: Some((client, server)),
            ..Default::default()
        }
    }

    pub fn type_error(sender: &'a str, receiver: &'a str) -> Self {
        Self {
            warning: Some(false),
            scope: Some(ProblemScopeType::Type),
            levels: Some((sender, receiver)),
            ..Default::default()
        }
    }

    pub fn type_warning(sender: &'a str, receiver: &'a str) -> Self {
        Self {
            warning: Some(true),
            scope: Some(ProblemScopeType::Type),
            levels: Some((sender, receiver)),
            ..Default::default()
        }
    }

    pub fn message(self, message: impl Into<StringPattern<'a>>) -> Self {
        Self { message: Some(message.into()), ..self }
    }

    pub fn path(self, path: StringPattern<'a>) -> Self {
        Self { path: Some(path), ..self }
    }
}

#[cfg(test)]
impl std::fmt::Debug for ProblemPattern<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut d = f.debug_struct("ProblemPattern");

        if let Some(warning) = self.warning {
            d.field("warning", &warning);
        };
        if let Some(message) = &self.message {
            d.field("message", message);
        }
        if let Some(scope) = &self.scope {
            d.field("scope", scope);
        }
        if let Some(path) = &self.path {
            d.field("path", path);
        }
        if let Some(levels) = self.levels {
            d.field("levels", &levels);
        }

        d.finish()
    }
}
