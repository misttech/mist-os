// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    analytics_command, return_user_error, user_error, Error, FfxContext, MetricsSession, Result,
};
use argh::{ArgsInfo, FromArgs};
use camino::Utf8PathBuf;
use ffx_command_error::bug;
use ffx_config::environment::ExecutableKind;
use ffx_config::logging::LogDestination;
use ffx_config::{AssertNoEnvError, EnvironmentContext, FfxConfigBacked};
use ffx_metrics::{enhanced_analytics, sanitize};
use ffx_writer::Format;
use std::collections::HashMap;
use std::fmt::Write;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::str::FromStr;

/// The environment variable name used for overriding the command name in help
/// output.
pub const FFX_WRAPPER_INVOKE: &'static str = "FFX_WRAPPER_INVOKE";

#[derive(Clone, Debug, PartialEq)]
/// The relevant argument and environment variables necessary to parse or
/// reconstruct an ffx command invocation.
pub struct FfxCommandLine {
    pub command: Vec<String>,
    pub ffx_args: Vec<String>,
    pub global: Ffx,
}

impl FfxCommandLine {
    /// Construct the command from the system environment ([`std::env::args`] and [`std::env::var`]), using
    /// the FFX_WRAPPER_INVOKE environment variable to obtain the `wrapper_name`, if present. See [`FfxCommand::new`]
    /// for more information.
    pub fn from_env() -> Result<Self> {
        let argv = Vec::from_iter(std::env::args());
        let wrapper_name = std::env::var(FFX_WRAPPER_INVOKE).ok();
        Self::new(wrapper_name.as_deref(), &argv)
    }

    /// Extract the command name from the given argument list, allowing for an overridden command name
    /// from a wrapper invocation so we provide useful information to the user. If the override has spaces, it will
    /// be split into multiple commands.
    pub fn new(wrapper_name: Option<&str>, argv: &[impl AsRef<str>]) -> Result<Self> {
        let mut args = argv.iter().map(AsRef::as_ref);
        let arg0 = args.next().ok_or_else(|| bug!("No first argument in argument vector"))?;
        let args = Vec::from_iter(args);
        let command =
            wrapper_name.map_or_else(|| vec![Self::base_cmd(&arg0)], |s| s.split(" ").collect());
        let global =
            Ffx::from_args(&command, &args).map_err(|err| Error::from_early_exit(&command, err))?;
        // the ffx args are the ones not including those captured by the ffx struct's remain vec.
        let ffx_args_len = args.len() - global.subcommand.len();
        let ffx_args = args.into_iter().take(ffx_args_len).map(str::to_owned).collect();
        let command = command.into_iter().map(str::to_owned).collect();
        Ok(Self { command, ffx_args, global })
    }

    /// Creates a string of the ffx part of the command, but with user-supplied parameter values removed
    /// for analytics. This only contains the top-level flags before any subcommands have been
    /// entered.
    pub fn redact_ffx_cmd(&self) -> Vec<String> {
        Ffx::redact_arg_values(
            &Vec::from_iter(self.cmd_iter()),
            &Vec::from_iter(self.ffx_args_iter()),
        )
        .expect("Already parsed args should be redactable")
    }

    /// Redacts the full command line using type `C` to decide how to redact the subcommand arguments.
    ///
    /// May panic if you try to use the wrong type `C`, so you should only use this after you've
    /// successfully parsed the arguments. That's why this takes a ref to the command struct in
    /// `_cmd` argument even though it doesn't use it, to make sure you've parsed it first.
    pub fn redact_subcmd<C: FromArgs>(&self, _cmd: &C) -> Vec<String> {
        let mut args = self.redact_ffx_cmd();
        let tool_cmd = Vec::from_iter(self.subcmd_iter().take(1));
        let tool_args = Vec::from_iter(self.subcmd_iter().skip(1));
        args.append(
            &mut C::redact_arg_values(&tool_cmd, &tool_args)
                .expect("Already parsed command line should redact ok"),
        );
        args
    }

    /// This produces an error type that will print help appropriate help output
    /// for what the command line looks like, and do the appropriate metrics
    /// logic.
    ///
    /// Note that both the Ok() and Err() returns of this are Errors. The Ok
    /// result is the proper help output, while the other kind of error is an
    /// error on metrics submission.
    pub async fn no_handler_help<T: crate::ToolSuite>(
        &self,
        metrics: MetricsSession,
        suite: &T,
    ) -> Result<Error> {
        if !analytics_command(&self.unredacted_args_for_analytics().join(" ")) {
            metrics.print_notice(&mut std::io::stderr()).await?;
        }

        let subcmd_name = self.global.subcommand.first();
        let help_err = match subcmd_name {
            Some(name) => {
                let mut output =
                    format!("Unknown ffx tool `{name}`. Did you mean one of the following?\n\n");
                suite.print_command_list(&mut output).await.ok();
                let code = 1;
                Error::Help { command: self.command.clone(), output, code }
            }
            None => {
                let help_err = Ffx::from_args(&Vec::from_iter(self.cmd_iter()), &["help"])
                    .expect_err("argh should always return help from a help command");
                let mut output = help_err.output;
                let code = help_err.status.map_or(1, |_| 0);
                writeln!(&mut output).ok();
                suite.print_command_list(&mut output).await.ok();
                Error::Help { command: self.command.clone(), output, code }
            }
        };
        let args_for_analytics: Vec<_> = match enhanced_analytics().await {
            // construct a 'sanitized' argument list that includes an indication of whether
            // it was just no arguments passed or an unknown subtool.
            false => self
                .redact_ffx_cmd()
                .into_iter()
                .chain(subcmd_name.map(|_| "<unknown-subtool>".to_owned()).into_iter())
                .collect(),
            true => self.unredacted_args_for_analytics(),
        };

        let error_res = match help_err.exit_code() {
            0 => Ok(ExitStatus::from_raw(0)),
            i @ _ => Err(Error::Help {
                command: self.command.clone(),
                output: help_err.to_string(),
                code: i,
            }),
        };

        metrics.command_finished(&error_res, &args_for_analytics).await?;
        Ok(help_err)
    }

    /// Creates a Vec<String> of full args from the commandline
    /// for use in enhanced analytics.
    pub fn unredacted_args_for_analytics(&self) -> Vec<String> {
        let mut v: Vec<String> = self.cmd_iter().into_iter().map(|v| v.to_string()).collect();
        let x: Vec<String> = self.ffx_args_iter().into_iter().map(|v| v.to_string()).collect();
        v.extend(x);
        v.extend(self.subcmd_iter().map(|v| v.to_string()));
        v.into_iter().map(|a| sanitize(&a)).collect()
    }

    /// Creates the command from the args directly. This is used when building JSON help information
    /// so that external commands are collected from the configuration in addition to the
    /// static subcommands.
    pub fn from_args_for_help(argv: &Vec<String>) -> Result<Self> {
        let wrapper = std::env::var(FFX_WRAPPER_INVOKE).ok();
        let wrapper_name = wrapper.as_deref();
        let mut args = argv.iter().map(AsRef::as_ref);
        let arg0 = args.next().ok_or_else(|| bug!("No first argument in argument vector"))?;
        let args = Vec::from_iter(args);
        let command =
            wrapper_name.map_or_else(|| vec![Self::base_cmd(&arg0)], |s| s.split(" ").collect());
        let global = Ffx::from_args_for_help(&args)?;
        // the ffx args are the ones not including those captured by the ffx struct's remain vec.
        let ffx_args_len = args.len() - global.subcommand.len();
        let ffx_args = args.into_iter().take(ffx_args_len).map(str::to_owned).collect();
        let command = command.into_iter().map(str::to_owned).collect();
        Ok(Self { command, ffx_args, global })
    }

    /// Returns an iterator of the command part of the command line
    pub fn cmd_iter<'a>(&'a self) -> impl Iterator<Item = &'a str> {
        self.command.iter().map(|s| s.as_str())
    }

    /// Returns an iterator of the command part of the command line
    pub fn ffx_args_iter<'a>(&'a self) -> impl Iterator<Item = &'a str> {
        self.ffx_args.iter().map(|s| s.as_str())
    }

    /// Returns an iterator of the subcommand and its arguments
    pub fn subcmd_iter<'a>(&'a self) -> impl Iterator<Item = &'a str> {
        self.global.subcommand.iter().map(String::as_str)
    }

    /// Returns an iterator of the whole command line
    pub fn all_iter<'a>(&'a self) -> impl Iterator<Item = &'a str> {
        self.cmd_iter().chain(self.ffx_args_iter()).chain(self.subcmd_iter())
    }

    /// Extract the base cmd from a path
    fn base_cmd(path: &str) -> &str {
        std::path::Path::new(path).file_name().map(|s| s.to_str()).flatten().unwrap_or(path)
    }
}

#[derive(ArgsInfo, Clone, Default, FfxConfigBacked, FromArgs, Debug, PartialEq)]
/// Fuchsia's developer tool
pub struct Ffx {
    #[argh(option, short = 'c')]
    /// override configuration values (key=value, JSON string, or path
    /// to a JSON config file)
    pub config: Vec<String>,

    #[argh(option, short = 'e')]
    /// override the path to the environment configuration file (file path)
    pub env: Option<String>,

    #[argh(option, hidden_help)]
    /// override the detection of the project root from which a config domain
    /// file is found (Warning: This is part of an experimental feature)
    pub env_root: Option<Utf8PathBuf>,

    #[argh(option)]
    /// produce output for a machine in the specified format; available formats: "json",
    /// "json-pretty"
    pub machine: Option<Format>,

    #[argh(switch)]
    /// produce the JSON schema for the MachineWriter output. The `--machine` option
    /// is required when producing the schema.
    pub schema: bool,

    #[argh(option)]
    /// create a stamp file at the given path containing the exit code
    pub stamp: Option<String>,

    #[argh(option, short = 't')]
    #[ffx_config_default("target.default")]
    /// apply operations across single or multiple targets
    pub target: Option<String>,

    #[argh(option)]
    #[ffx_config_default(key = "proxy.timeout_secs", default = "1.0")]
    /// override default proxy timeout (default is 1 second)
    pub timeout: Option<f64>,

    #[argh(option, short = 'l', long = "log-level")]
    #[ffx_config_default(key = "log.level", default = "Info")]
    /// sets the log level for ffx output (default = Info). Other possible values are Info, Error,
    /// Warn, and Trace. Can be persisted via log.level config setting.
    pub log_level: Option<String>,

    #[argh(option, long = "isolate-dir")]
    /// turn on isolation mode using the given directory to isolate all config and socket files into
    /// the specified directory. This overrides the FFX_ISOLATE_DIR env variable, which can also put
    /// ffx into this mode.
    pub isolate_dir: Option<PathBuf>,

    #[argh(switch, short = 'v', long = "verbose")]
    /// logs ffx output to stdio according to log level
    pub verbose: bool,

    #[argh(positional, greedy)]
    pub subcommand: Vec<String>,

    #[argh(option, short = 'o', long = "log-output")]
    /// specify destination of log output. "-" or "stdout" for stdout, "stderr"
    /// for stderr. If no destination is specified, log.dir will be used. If a
    /// destination is specified, then log.dir will be ignored.
    pub log_destination: Option<LogDestination>,

    #[argh(switch)]
    /// disables loading configuration from the file system and only uses
    /// configuration specified on the command line or the compiled in default values.
    /// Intended for use when running ffx as part of a hermetic build.
    pub no_environment: bool,

    #[argh(switch)]
    /// switch which enables the 'strict' feature set. When set, ffx has stricter
    /// behavioual patterns. Including but not limited to:
    ///   * Not doing discovery
    ///   * Not starting the daemon
    ///   * Communicating directly with the target from the ffx cli (not the daemon)
    ///   * Configuration is read only
    /// The features in this flag are currently under active development.
    pub strict: bool,
}

#[derive(thiserror::Error, PartialEq, Debug)]
enum StrictCheckError {
    #[error("Command line flags unsatisfactory for strict mode:{}", format_strict_check_error_enums(.0))]
    User(Vec<StrictCheckErrorEnum>),
}

#[derive(thiserror::Error, Debug, PartialEq, Clone)]
enum StrictCheckErrorEnum {
    #[error("ffx strict requires that the machine writer be specified")]
    MustHaveMachineSpecified,
    #[error("ffx strict requires that the target be explicitly specified")]
    MustHaveTarget,
    #[error("ffx strict requires that the Target be specified by address. Actually passed: \"{}\"", .0)]
    TargetMustBeAddress(String),
    #[error("ffx strict requires that the Target be a valid IP address. Invalid ID: \"{}\"", .0)]
    TargetAddressMustHaveValidScopeId(String),
    #[error("When running in strict mode, config flags must be list of Key Value Pairs or valid JSON. Passed: \"{}\"", .0)]
    ConfigArgMustBeJsonOrKeyValuePair(String),
    #[error("Specifying strict mode and isolate dir are mutually exclusive. specify one or the other. Isolate Dir Passed: {}", .0.display())]
    StrictAndIsolateMutuallyExclusive(PathBuf),
    #[error("ffx strict requires that the Log Destination be explicitly specified")]
    MustHaveLogDestination,
}

fn format_strict_check_error_enums(errors: &Vec<StrictCheckErrorEnum>) -> String {
    if errors.len() < 1 {
        return "An error occurred formatting the list of StrictCheckError's. Expected more than 1 error. Got 0".to_string();
    }
    let error_string =
        errors.clone().into_iter().map(|e| e.to_string()).collect::<Vec<_>>().join("\n\t");
    "\n\t".to_owned() + &error_string
}

/// When a tool is run in "strict" mode there are certain constraints on passed
/// arguments. This ensures they are all satisfied
pub fn check_strict_constraints(ffx: &Ffx, requires_target: bool) -> Result<()> {
    // In this case we're not in strict mode so we just exit out
    if !ffx.strict {
        return Ok(());
    }

    let mut errors = vec![];

    match (ffx.strict, &ffx.isolate_dir) {
        (true, Some(isolate_dir)) => {
            errors.push(StrictCheckErrorEnum::StrictAndIsolateMutuallyExclusive(
                isolate_dir.to_path_buf(),
            ));
        }
        _ => {}
    }

    if ffx.machine.is_none() {
        errors.push(StrictCheckErrorEnum::MustHaveMachineSpecified);
    }

    if ffx.log_destination.is_none() {
        errors.push(StrictCheckErrorEnum::MustHaveLogDestination);
    }

    if requires_target {
        match &ffx.target {
            None => errors.push(StrictCheckErrorEnum::MustHaveTarget),
            Some(t) => match netext::parse_address_parts(t.as_str()) {
                Err(_) => errors.push(StrictCheckErrorEnum::TargetMustBeAddress(t.clone())),
                Ok((_, scope, _)) => {
                    if let Some(scope) = scope {
                        match netext::get_verified_scope_id(scope) {
                            Ok(_) => {}
                            Err(_) => {
                                errors.push(
                                    StrictCheckErrorEnum::TargetAddressMustHaveValidScopeId(
                                        scope.to_string(),
                                    ),
                                );
                            }
                        };
                    };
                }
            },
        };
    }

    for potential_config in ffx.config.iter() {
        if ffx_config::runtime::try_parse_json(potential_config).is_err()
            && ffx_config::runtime::try_split_name_value_pairs(potential_config).is_err()
        {
            errors.push(StrictCheckErrorEnum::ConfigArgMustBeJsonOrKeyValuePair(
                potential_config.clone(),
            ));
        }
    }

    if errors.len() > 0 {
        return_user_error!(StrictCheckError::User(errors));
    }

    Ok(())
}

impl Ffx {
    pub fn load_context(&self, exe_kind: ExecutableKind) -> Result<EnvironmentContext> {
        let env_vars =
            if self.strict { HashMap::new() } else { HashMap::from_iter(std::env::vars()) };
        self.load_context_with_env(exe_kind, env_vars)
    }

    pub fn load_context_with_env(
        &self,
        exe_kind: ExecutableKind,
        env_vars: HashMap<String, String>,
    ) -> Result<EnvironmentContext> {
        // Configuration initialization must happen before ANY calls to the config (or the cache won't
        // properly have the runtime parameters.
        let overrides = self.runtime_config_overrides();
        let runtime_args = ffx_config::runtime::populate_runtime(&*self.config, overrides)?;
        let env_path = self.env.as_ref().map(PathBuf::from);
        let current_dir = std::env::current_dir().bug_context("Failed to get working directory")?;

        // If we're given an isolation setting, use that. Otherwise do a normal detection of the environment.
        match (self, env_vars.get("FFX_ISOLATE_DIR").map(PathBuf::from)) {
            (Ffx { strict: true, .. }, _) => {
                match EnvironmentContext::strict(exe_kind, runtime_args) {
                    Ok(env) => Ok(env),
                    // TODO(b/368047122): This is some unfortunately awkward error conversion code that
                    // can't be done inside the config library as implementing
                    // `From<ffx_command::Error>` would create a circular dependency. Ideally the
                    // `ffx_command::Error` struct should just be put into its own micro-crate in order
                    // to deal with this issue.
                    //
                    // The long and short of it is: it's a user error if env variables were found, a
                    // bug if it's anything else.
                    Err(root_error) => match root_error.downcast::<AssertNoEnvError>() {
                        Ok(anee) => match anee {
                            u @ AssertNoEnvError::Unexpected(_) => {
                                Err(anyhow::Error::from(u).into())
                            }
                            evf @ AssertNoEnvError::EnvVariablesFound(..) => {
                                Err(crate::Error::User(evf.into()))
                            }
                        },
                        Err(e) => Err(e.into()),
                    },
                }
            }
            (Ffx { env_root: Some(domain_root), isolate_dir: Some(isolate_root), .. }, _) => {
                EnvironmentContext::config_domain_root(
                    exe_kind,
                    domain_root.clone(),
                    runtime_args,
                    Some(isolate_root.clone()),
                    self.no_environment,
                )
                .map_err(Into::into)
            }
            (Ffx { env_root: Some(domain_root), .. }, isolate_root) => {
                EnvironmentContext::config_domain_root(
                    exe_kind,
                    domain_root.clone(),
                    runtime_args,
                    isolate_root,
                    self.no_environment,
                )
                .map_err(Into::into)
            }
            (Ffx { isolate_dir: Some(ref path), .. }, _) | (_, Some(ref path)) => {
                EnvironmentContext::isolated(
                    exe_kind,
                    path.clone(),
                    env_vars,
                    runtime_args,
                    env_path,
                    Utf8PathBuf::try_from(current_dir).ok().as_deref(),
                    self.no_environment,
                )
                .map_err(Into::into)
            }
            _ => EnvironmentContext::detect(
                exe_kind,
                runtime_args,
                &current_dir,
                env_path,
                self.no_environment,
            )
            .map_err(|e| user_error!(e)),
        }
    }

    /// Appends information about there being more commands available if run in
    /// a different way. Used in contexts where we can't get the list of
    /// commands because we couldn't parse the command line correctly.
    pub fn more_commands_help(output: &mut impl Write, cmd: &str) -> std::fmt::Result {
        writeln!(
            output,
            "Note: There may be more commands available, use `{cmd} commands` for a complete list."
        )?;
        writeln!(output, "See '{cmd} <command> help' for more information on a specific command.")
    }

    /// Constructs a Ffx object from the given argv.
    /// This is done since argh parsing will return an
    /// error if the command help should be returned.
    ///
    /// In order to get the ArgsInfo data for the command,
    /// construct the Ffx args so we have the global command
    /// options and subcommand separated.
    pub fn from_args_for_help(argv: &Vec<&str>) -> Result<Self> {
        let mut return_val = Self {
            config: vec![],
            env: None,
            env_root: None,
            machine: None,
            schema: false,
            stamp: None,
            target: None,
            timeout: None,
            log_level: None,
            isolate_dir: None,
            verbose: false,
            subcommand: vec![],
            log_destination: None,
            no_environment: false,
            strict: false,
        };

        let mut argv_iter = argv.iter();
        while let Some(opt) = argv_iter.next() {
            match *opt {
                "-c" | "--config" => {
                    if let Some(val) = argv_iter.next() {
                        return_val.config.push(val.to_string());
                    }
                }
                "-e" | "--env" => {
                    if let Some(val) = argv_iter.next() {
                        return_val.env = Some(val.to_string());
                    }
                }
                "--env-root" => {
                    if let Some(val) = argv_iter.next() {
                        return_val.env_root = Some(Utf8PathBuf::from(val));
                    }
                }
                "--machine" => {
                    if let Some(val) = argv_iter.next() {
                        if let Ok(fmt) = ffx_writer::Format::from_str(val) {
                            return_val.machine = Some(fmt);
                        }
                    }
                }
                "--schema" => {
                    return_val.schema = true;
                }
                "--stamp" => {
                    if let Some(val) = argv_iter.next() {
                        return_val.stamp = Some(val.to_string());
                    }
                }
                "-t" | "--target" => {
                    if let Some(val) = argv_iter.next() {
                        return_val.target = Some(val.to_string());
                    }
                }
                "--timeout" => {
                    if let Some(val) = argv_iter.next() {
                        return_val.timeout = val.to_string().parse::<f64>().ok();
                    }
                }
                "-l" | "--log-level" => {
                    if let Some(val) = argv_iter.next() {
                        return_val.log_level = Some(val.to_string());
                    }
                }
                "--isolate-dir" => {
                    if let Some(val) = argv_iter.next() {
                        return_val.isolate_dir = Some(PathBuf::from(val));
                    }
                }
                "-v" | "--verbose" => {
                    return_val.verbose = true;
                }
                "-o" | "--log-output" => {
                    if let Some(val) = argv_iter.next() {
                        // Unwrap is okay because LogDestination::Err is `Infallible`
                        return_val.log_destination = Some(LogDestination::from_str(val).unwrap());
                    }
                }
                "--no-environment" => {
                    return_val.no_environment = true;
                }
                "--strict" => {
                    return_val.strict = true;
                }
                _ => {
                    return_val.subcommand.push(opt.to_string());
                    return_val.subcommand.extend(argv_iter.clone().map(|s| s.to_string()));
                    break;
                }
            }
        }

        Ok(return_val)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    use assert_matches::assert_matches;
    use ffx_config::environment::EnvironmentKind;
    use std::io::Write;
    use tempfile::{tempdir, TempDir};

    #[test]
    fn test_check_ffx_strict() {
        struct TestCase {
            inputs: Vec<&'static str>,
            name: String,
            expected_errors: Vec<StrictCheckErrorEnum>,
        }

        let cases = vec![
            TestCase {
                inputs: vec![
                    "ffx",
                    "--strict",
                    "--target",
                    "192.168.1.1:8001",
                    "--config",
                    "foo=bar,baz=biz",
                    "--config",
                    "{\"key\":\"valid_json\"}",
                    "--log-output",
                    "/tmp/out.log",
                    "target",
                    "echo",
                ],
                name: "Missing Machine".into(),
                expected_errors: vec![StrictCheckErrorEnum::MustHaveMachineSpecified],
            },
            TestCase {
                inputs: vec![
                    "ffx",
                    "--strict",
                    "--log-output",
                    "/tmp/out.log",
                    "--machine",
                    "json",
                    "target",
                    "echo",
                ],
                name: "Missing Target Name".into(),
                expected_errors: vec![StrictCheckErrorEnum::MustHaveTarget],
            },
            TestCase {
                inputs: vec![
                    "ffx",
                    "--strict",
                    "--target",
                    "192.168.1.1:8004",
                    "--log-output",
                    "/tmp/out.log",
                    "--config",
                    "asdf",
                    "--machine",
                    "json",
                    "target",
                    "echo",
                ],
                name: "Bad config setting".into(),
                expected_errors: vec![StrictCheckErrorEnum::ConfigArgMustBeJsonOrKeyValuePair(
                    "asdf".into(),
                )],
            },
            TestCase {
                inputs: vec![
                    "ffx",
                    "--strict",
                    "--log-output",
                    "/tmp/out.log",
                    "--target",
                    "no waaaaaayyy",
                    "--machine",
                    "json",
                    "target",
                    "echo",
                ],
                name: "Target must be SocketAddr".into(),
                expected_errors: vec![StrictCheckErrorEnum::TargetMustBeAddress(
                    "no waaaaaayyy".into(),
                )],
            },
            TestCase {
                inputs: vec![
                    "ffx",
                    "--strict",
                    "--isolate-dir",
                    "/tmp/foo",
                    "--machine",
                    "json",
                    "--target",
                    "193.168.1.1:8081",
                    "--log-output",
                    "/tmp/out.log",
                    "target",
                    "echo",
                ],
                name: "Strict and Isolate Mutually Exclusive".into(),
                expected_errors: vec![StrictCheckErrorEnum::StrictAndIsolateMutuallyExclusive(
                    PathBuf::from("/tmp/foo"),
                )],
            },
        ];

        for case in cases {
            let cmd_line =
                FfxCommandLine::new(None, &case.inputs).expect("Command line should parse");
            let res = check_strict_constraints(&cmd_line.global, true);
            assert!(res.is_err(), "Test Case {} was not an error", case.name);

            let Error::User(got_err) = res.unwrap_err() else { panic!() };
            match got_err.downcast_ref::<StrictCheckError>() {
                Some(StrictCheckError::User(inner_errs)) => {
                    assert_eq!(
                        case.expected_errors, *inner_errs,
                        "Test Case {} had the wrong errors",
                        case.name
                    );
                }
                _ => {
                    unreachable!();
                }
            }
        }
    }

    #[test]
    fn cmd_only_last_component() {
        let args = ["test/things/ffx", "--verbose"].map(String::from);
        let cmd_line = FfxCommandLine::new(None, &args).expect("Command line should parse");
        assert_eq!(cmd_line.command, vec!["ffx"]);
        assert_eq!(cmd_line.ffx_args, vec!["--verbose"]);
    }

    #[test]
    fn cmd_override_invoke() {
        let args = ["test/things/ffx", "--verbose"].map(String::from);
        let cmd_line =
            FfxCommandLine::new(Some("tools/ffx"), &args).expect("Command line should parse");
        assert_eq!(cmd_line.command, vec!["tools/ffx"]);
        assert_eq!(cmd_line.ffx_args, vec!["--verbose"]);
    }

    #[test]
    fn cmd_override_multiple_terms_invoke() {
        let args = ["test/things/ffx", "--verbose"].map(String::from);
        let cmd_line =
            FfxCommandLine::new(Some("fx ffx"), &args).expect("Command line should parse");
        assert_eq!(cmd_line.command, vec!["fx", "ffx"]);
        assert_eq!(cmd_line.ffx_args, vec!["--verbose"]);
    }

    #[test]
    fn test_cmd_for_help() {
        let args = ["test/things/ffx", "--verbose", "--machine", "json-pretty"]
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>();
        let cmd_for_help =
            FfxCommandLine::from_args_for_help(&args).expect("Command line should parse");
        assert_eq!(cmd_for_help.ffx_args, vec!["--verbose", "--machine", "json-pretty"]);
        assert!(cmd_for_help.global.verbose);
        assert!(cmd_for_help.global.machine == Some(Format::JsonPretty));
    }

    /// A subcommand
    #[derive(FromArgs, Default)]
    #[argh(subcommand, name = "subcommand")]
    #[allow(unused)]
    struct TestCmd {
        /// an argument
        #[argh(switch)]
        arg: bool,
        /// another argument
        #[argh(option)]
        stuff: String,
    }

    #[test]
    fn redact_ffx_args() {
        let args = ["ffx", "-v", "--env", "boom", "subcommand", "--arg"];
        let cmd_line = FfxCommandLine::new(None, &args).expect("Command line should parse");
        assert_eq!(cmd_line.command, vec!["ffx"]);
        assert_eq!(cmd_line.ffx_args, vec!["-v", "--env", "boom"]);
        assert_eq!(cmd_line.redact_ffx_cmd(), vec!["ffx", "--env", "-v"]);
    }

    #[test]
    fn redact_subcmd_args() {
        let args = ["ffx", "-v", "--env", "boom", "subcommand", "--arg", "--stuff", "wee"];
        let cmd_line = FfxCommandLine::new(None, &args).expect("Command line should parse");
        assert_eq!(cmd_line.global.subcommand, vec!["subcommand", "--arg", "--stuff", "wee"]);
        assert_eq!(
            cmd_line.redact_subcmd(&TestCmd::default()),
            vec!["ffx", "--env", "-v", "subcommand", "--arg", "--stuff"]
        );
    }

    fn simple_config_domain_root() -> TempDir {
        let root = tempdir().expect("domain context root directory");
        std::fs::File::create(root.path().join("fuchsia_env.toml"))
            .expect("fuchsia_env.toml")
            .write_all(b"[fuchsia]")
            .expect("fuchsia section");
        root
    }

    #[test]
    fn test_load_config_domain_context() {
        let domain_root = simple_config_domain_root();
        let ffx = Ffx {
            env_root: Some(domain_root.path().join("fuchsia_env.toml").try_into().unwrap()),
            ..Default::default()
        };
        let context = ffx
            .load_context_with_env(ExecutableKind::Test, Default::default())
            .expect("domain context");
        assert_matches!(
            context.env_kind(),
            EnvironmentKind::ConfigDomain { isolate_root: None, .. }
        );
    }

    #[test]
    fn test_load_isolated_arg_config_domain_context() {
        let domain_root = simple_config_domain_root();
        let isolate_dir = tempdir().expect("isolate dir");
        let ffx = Ffx {
            env_root: Some(domain_root.path().join("fuchsia_env.toml").try_into().unwrap()),
            isolate_dir: Some(isolate_dir.path().to_owned()),
            ..Default::default()
        };
        let context = ffx
            .load_context_with_env(ExecutableKind::Test, Default::default())
            .expect("domain context");
        assert_matches!(
            context.env_kind(),
            EnvironmentKind::ConfigDomain { isolate_root: Some(_), .. }
        );
    }

    #[test]
    fn test_load_isolated_env_config_domain_context() {
        let domain_root = simple_config_domain_root();
        let isolate_dir = tempdir().expect("isolate dir");
        let isolate_dir_str = isolate_dir.path().to_string_lossy().to_string();
        let ffx = Ffx {
            env_root: Some(domain_root.path().join("fuchsia_env.toml").try_into().unwrap()),
            ..Default::default()
        };
        let env_vars = HashMap::from_iter([("FFX_ISOLATE_DIR".to_owned(), isolate_dir_str)]);
        let context =
            ffx.load_context_with_env(ExecutableKind::Test, env_vars).expect("domain context");
        assert_matches!(
            context.env_kind(),
            EnvironmentKind::ConfigDomain { isolate_root: Some(_), .. }
        );
    }

    #[test]
    fn test_load_isolated_arg_overriding_env_config_domain_context() {
        let domain_root = simple_config_domain_root();
        let isolate_dir = tempdir().expect("isolate dir");
        let isolate_dir_str = isolate_dir.path().to_string_lossy().to_string();
        let ffx = Ffx {
            env_root: Some(domain_root.path().join("fuchsia_env.toml").try_into().unwrap()),
            isolate_dir: Some(isolate_dir.path().to_owned()),
            ..Default::default()
        };
        let env_vars: HashMap<String, String> =
            [("FFX_ISOLATE_DIR".to_owned(), "/dev/zero".to_owned())].into_iter().collect();
        let context =
            ffx.load_context_with_env(ExecutableKind::Test, env_vars).expect("domain context");
        assert_matches!(context.env_kind(), EnvironmentKind::ConfigDomain { isolate_root: Some(root), .. } if root == &PathBuf::from(&isolate_dir_str));
    }

    #[test]
    // This tests that new options do not break the manual parsing of partial command lines or
    // command lines that result in help output.
    fn test_cmd_for_help_long_flags() {
        let options = Ffx::get_args_info();
        let mut all_args = vec![];
        for opt in options.flags {
            match opt.kind {
                argh::FlagInfoKind::Switch => {
                    if opt.long != "--help" {
                        all_args.push(opt.long);
                    }
                }
                argh::FlagInfoKind::Option { arg_name } => match opt.long {
                    "--machine" => {
                        all_args.push(opt.long);
                        all_args.push("json");
                    }
                    "--timeout" => {
                        all_args.push(opt.long);
                        all_args.push("123");
                    }
                    "--strict" => {
                        all_args.push(opt.long);
                        all_args.push("false");
                    }
                    _ => {
                        all_args.push(opt.long);
                        all_args.push(arg_name);
                    }
                },
            }
        }
        let _ffx = Ffx::from_args(&["ffx"], &all_args).expect("parsing all long args");
        let _ffx_for_help = Ffx::from_args_for_help(&all_args).expect("parsing args for help");
        assert_eq!(_ffx, _ffx_for_help);
    }

    #[test]
    // This tests that new options do not break the manual parsing of partial command lines or
    // command lines that result in help output.
    fn test_cmd_for_help_short_flags() {
        let options = Ffx::get_args_info();
        let mut all_arg_strings: Vec<String> = vec![];
        for opt in options.flags {
            match opt.kind {
                argh::FlagInfoKind::Switch => {
                    if opt.long != "--help" {
                        if let Some(s) = opt.short {
                            all_arg_strings.push(format!("-{s}"));
                        }
                    }
                }
                argh::FlagInfoKind::Option { arg_name } => match opt.long {
                    "--machine" => {
                        if let Some(s) = opt.short {
                            all_arg_strings.push(format!("-{s}"));
                        }
                        all_arg_strings.push("json".into());
                    }
                    "--timeout" => {
                        if let Some(s) = opt.short {
                            all_arg_strings.push(format!("-{s}"));
                        }
                        all_arg_strings.push("123".into());
                    }
                    _ => {
                        if let Some(s) = opt.short {
                            all_arg_strings.push(format!("-{s}"));
                        }
                        all_arg_strings.push(arg_name.into());
                    }
                },
            }
        }
        let all_args: Vec<&str> = all_arg_strings.iter().map(|e| e.as_str()).collect();
        let _ffx = Ffx::from_args(&["ffx"], &all_args).expect("parsing all long args");
        let _ffx_for_help = Ffx::from_args_for_help(&all_args).expect("parsing args for help");
        assert_eq!(_ffx, _ffx_for_help);
    }
}
