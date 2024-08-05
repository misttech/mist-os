// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use std::fmt::Display;

use super::Path;

// Helper to find the common prefix of two strings and return the common prefix and the differing suffixes.
fn common_prefix(a: &str, b: &str) -> (String, String, String) {
    let common: String =
        a.chars().zip(b.chars()).take_while(|(a, b)| a == b).map(|(a, _)| a).collect();
    let common_length = common.chars().count();
    (common, a.chars().skip(common_length).collect(), b.chars().skip(common_length).collect())
}

#[derive(PartialEq, PartialOrd, Eq, Ord, Debug, Clone, Copy)]
pub enum CompatibilityDegree {
    Incompatible,
    WeaklyCompatible,
    StronglyCompatible,
}

#[derive(PartialEq, Eq, Debug, Ord, PartialOrd)]
enum ProblemPathKind {
    AbiSurface,
    Protocol,
    Type,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ProblemPaths {
    AbiSurface { external: Path, platform: Path },
    Protocol { client: Path, server: Path },
    Type { sender: Path, receiver: Path },
}

impl ProblemPaths {
    fn kind(&self) -> ProblemPathKind {
        use ProblemPathKind::*;
        match self {
            ProblemPaths::AbiSurface { external: _, platform: _ } => AbiSurface,
            ProblemPaths::Protocol { client: _, server: _ } => Protocol,
            ProblemPaths::Type { sender: _, receiver: _ } => Type,
        }
    }
    fn paths(&self) -> (&Path, &Path) {
        match self {
            ProblemPaths::AbiSurface { external, platform } => (external, platform),
            ProblemPaths::Protocol { client, server } => (client, server),
            ProblemPaths::Type { sender, receiver } => (sender, receiver),
        }
    }
    fn path_for_display(&self) -> String {
        let (a, b) = self.paths();
        let (a, b) = (a.string(), b.string());
        if a == b {
            a
        } else {
            let (c, a_suffix, b_suffix) = common_prefix(&a, &b);
            if a_suffix.is_empty() {
                b.to_string()
            } else if b_suffix.is_empty() {
                a.to_string()
            } else {
                format!("{c}({a_suffix}|{b_suffix})")
            }
        }
    }
    fn path_for_comparison(&self) -> String {
        let (a, b) = self.paths();
        let (a, b) = (a.string(), b.string());
        std::cmp::min(a, b)
    }
    fn levels(&self) -> (&str, &str) {
        match self {
            ProblemPaths::AbiSurface { external, platform } => {
                (external.api_level(), platform.api_level())
            }
            ProblemPaths::Protocol { client, server } => (client.api_level(), server.api_level()),
            ProblemPaths::Type { sender, receiver } => (sender.api_level(), receiver.api_level()),
        }
    }
}

impl Display for ProblemPaths {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.path_for_display())
    }
}

impl PartialOrd for ProblemPaths {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        let a: String = self.path_for_comparison().into();
        let b: String = other.path_for_comparison().into();
        Some(cmp_prefixes_later(&a, &b))
    }
}

impl Ord for ProblemPaths {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match self.partial_cmp(other) {
            Some(ord) => ord,
            None => self.kind().cmp(&other.kind()),
        }
    }
}

fn cmp_prefixes_later(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    // Doing a string comparison, but sorting longer strings before their prefixes.
    // This way more specific errors show up before more general ones.
    match a.cmp(b) {
        Equal => Equal,
        Less => {
            if b.starts_with(a) {
                Greater
            } else {
                Less
            }
        }
        Greater => {
            if a.starts_with(b) {
                Less
            } else {
                Greater
            }
        }
    }
}

#[test]
fn test_cmp_prefixes_later() {
    use std::cmp::Ordering::*;
    assert_eq!(Equal, cmp_prefixes_later("foo", "foo"));
    assert_eq!(Greater, cmp_prefixes_later("foo", "bar"));
    assert_eq!(Less, cmp_prefixes_later("bar", "foo"));
    assert_eq!(Greater, cmp_prefixes_later("foo", "foo.bar"));
    assert_eq!(Less, cmp_prefixes_later("foo.bar", "foo"));
}

#[derive(Clone, PartialEq, Eq)]
pub struct CompatibilityProblem {
    paths: ProblemPaths,
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
        writeln!(f, " at: {}", self.paths)
    }
}

impl std::fmt::Debug for CompatibilityProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CompatibilityProblem::")?;
        match self.paths.kind() {
            ProblemPathKind::AbiSurface => {
                write!(f, "platform")?;
                assert!(!self.warning);
            }
            ProblemPathKind::Protocol => {
                write!(f, "protocol")?;
                assert!(!self.warning);
            }
            ProblemPathKind::Type => {
                if self.warning {
                    write!(f, "type_warning")?;
                } else {
                    write!(f, "type_error")?;
                }
            }
        }
        let levels = self.paths.levels();
        write!(f, "({:?}, {:?})", levels.0, levels.1)?;
        write!(f, "{{ path={:?}, message={:?} }}", self.paths.path_for_display(), self.message)?;
        Ok(())
    }
}

impl PartialOrd for CompatibilityProblem {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        match self.warning.partial_cmp(&other.warning) {
            Some(core::cmp::Ordering::Equal) => {}
            ord => return ord,
        }
        match self.paths.partial_cmp(&other.paths) {
            Some(core::cmp::Ordering::Equal) => {}
            ord => return ord,
        }
        None
    }
}

impl Ord for CompatibilityProblem {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.warning.cmp(&other.warning)
    }
}

#[test]
fn test_compatibility_problem_comparison() {
    let warning = CompatibilityProblem {
        paths: ProblemPaths::AbiSurface { external: Path::empty(), platform: Path::empty() },
        warning: true,
        message: "beware".to_owned(),
    };
    let error = CompatibilityProblem {
        paths: ProblemPaths::AbiSurface { external:Path::empty(), platform: Path::empty() },
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
            paths: ProblemPaths::AbiSurface {
                external: external.clone(),
                platform: platform.clone(),
            },
            warning: false,
            message: message.as_ref().to_owned(),
        });
    }
    pub fn protocol(&mut self, client: &Path, server: &Path, message: impl AsRef<str>) {
        self.0.push(CompatibilityProblem {
            paths: ProblemPaths::Protocol { client: client.clone(), server: server.clone() },
            warning: false,
            message: message.as_ref().to_owned(),
        });
    }

    pub fn type_error(&mut self, sender: &Path, receiver: &Path, message: impl AsRef<str>) {
        self.0.push(CompatibilityProblem {
            paths: ProblemPaths::Type { sender: sender.clone(), receiver: receiver.clone() },
            warning: false,
            message: message.as_ref().to_owned(),
        });
    }
    pub fn type_warning(&mut self, sender: &Path, receiver: &Path, message: impl AsRef<str>) {
        self.0.push(CompatibilityProblem {
            paths: ProblemPaths::Type { sender: sender.clone(), receiver: receiver.clone() },
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
        self.has_problems(vec![])
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

    pub fn into_errors_and_warnings(self) -> (Self, Self) {
        let (warnings, errors) = self.0.into_iter().partition(|p| p.warning);
        (Self(errors), Self(warnings))
    }

    pub fn len(&self) -> usize {
        self.0.len()
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
    kind: Option<ProblemPathKind>,
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
        if let Some(kind) = &self.kind {
            if kind != &problem.paths.kind() {
                return false;
            }
        }
        if let Some(path) = &self.path {
            if !path.matches(problem.paths.paths().0.string())
                && !path.matches(problem.paths.paths().1.string())
            {
                return false;
            }
        }
        if let Some(levels) = self.levels {
            if levels != problem.paths.levels() {
                return false;
            }
        }
        true
    }
    pub fn platform() -> Self {
        Self { warning: Some(false), kind: Some(ProblemPathKind::AbiSurface), ..Default::default() }
    }
    pub fn protocol(client: &'a str, server: &'a str) -> Self {
        Self {
            warning: Some(false),
            kind: Some(ProblemPathKind::Protocol),
            levels: Some((client, server)),
            ..Default::default()
        }
    }

    pub fn type_error(sender: &'a str, receiver: &'a str) -> Self {
        Self {
            warning: Some(false),
            kind: Some(ProblemPathKind::Type),
            levels: Some((sender, receiver)),
            ..Default::default()
        }
    }

    pub fn type_warning(sender: &'a str, receiver: &'a str) -> Self {
        Self {
            warning: Some(true),
            kind: Some(ProblemPathKind::Type),
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
        if let Some(kind) = &self.kind {
            d.field("kind", kind);
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
