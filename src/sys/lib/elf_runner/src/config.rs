// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::error::ProgramError, ::routing::policy::ScopedPolicyChecker, fidl_fuchsia_data as fdata,
    fuchsia_zircon as zx, runner::StartInfoProgramError,
};

const CREATE_RAW_PROCESSES_KEY: &str = "job_policy_create_raw_processes";
const SHARED_PROCESS_KEY: &str = "is_shared_process";
const CRITICAL_KEY: &str = "main_process_critical";
const FORWARD_STDOUT_KEY: &str = "forward_stdout_to";
const FORWARD_STDERR_KEY: &str = "forward_stderr_to";
const VMEX_KEY: &str = "job_policy_ambient_mark_vmo_exec";
const STOP_EVENT_KEY: &str = "lifecycle.stop_event";
const STOP_EVENT_VARIANTS: [&'static str; 2] = ["notify", "ignore"];
const USE_NEXT_VDSO_KEY: &str = "use_next_vdso";

/// Target sink for stdout and stderr output streams.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamSink {
    /// The component omitted configuration or explicitly requested forwarding to the log.
    Log,
    /// The component requested to not forward to the log.
    None,
}

impl Default for StreamSink {
    fn default() -> Self {
        StreamSink::Log
    }
}

/// Parsed representation of the `ComponentStartInfo.program` dictionary.
#[derive(Debug, Default, Eq, PartialEq)]
pub struct ElfProgramConfig {
    pub binary: String,
    pub args: Vec<String>,
    pub notify_lifecycle_stop: bool,
    pub ambient_mark_vmo_exec: bool,
    pub main_process_critical: bool,
    pub create_raw_processes: bool,
    pub is_shared_process: bool,
    pub use_next_vdso: bool,
    pub stdout_sink: StreamSink,
    pub stderr_sink: StreamSink,
    pub environ: Option<Vec<String>>,
}

impl ElfProgramConfig {
    /// Parse the given dictionary into an ElfProgramConfig, checking it against security policy as
    /// needed.
    ///
    /// Checking against security policy is intentionally combined with parsing here, so that policy
    /// enforcement is as close to the point of parsing as possible and can't be inadvertently skipped.
    pub fn parse_and_check(
        program: &fdata::Dictionary,
        checker: &ScopedPolicyChecker,
    ) -> Result<Self, ProgramError> {
        let config = Self::parse(program).map_err(ProgramError::Parse)?;

        if config.ambient_mark_vmo_exec {
            checker.ambient_mark_vmo_exec_allowed().map_err(ProgramError::Policy)?;
        }

        if config.main_process_critical {
            checker.main_process_critical_allowed().map_err(ProgramError::Policy)?;
        }

        if config.create_raw_processes {
            checker.create_raw_processes_allowed().map_err(ProgramError::Policy)?;
        }

        if config.is_shared_process && !config.create_raw_processes {
            return Err(ProgramError::SharedProcessRequiresJobPolicy);
        }

        Ok(config)
    }

    // Parses a `program` dictionary but does not check policy or consistency.
    fn parse(program: &fdata::Dictionary) -> Result<Self, StartInfoProgramError> {
        let notify_lifecycle_stop =
            match runner::get_enum(program, STOP_EVENT_KEY, &STOP_EVENT_VARIANTS)? {
                Some("notify") => true,
                _ => false,
            };

        Ok(ElfProgramConfig {
            binary: runner::get_program_binary_from_dict(&program)?,
            args: runner::get_program_args_from_dict(&program),
            notify_lifecycle_stop,
            ambient_mark_vmo_exec: runner::get_bool(program, VMEX_KEY)?,
            main_process_critical: runner::get_bool(program, CRITICAL_KEY)?,
            create_raw_processes: runner::get_bool(program, CREATE_RAW_PROCESSES_KEY)?,
            is_shared_process: runner::get_bool(program, SHARED_PROCESS_KEY)?,
            use_next_vdso: runner::get_bool(program, USE_NEXT_VDSO_KEY)?,
            stdout_sink: get_stream_sink(&program, FORWARD_STDOUT_KEY)?,
            stderr_sink: get_stream_sink(&program, FORWARD_STDERR_KEY)?,
            environ: runner::get_environ(&program)?,
        })
    }

    pub fn process_options(&self) -> zx::ProcessOptions {
        if self.is_shared_process {
            zx::ProcessOptions::SHARED
        } else {
            zx::ProcessOptions::empty()
        }
    }
}

fn get_stream_sink(
    dict: &fdata::Dictionary,
    key: &str,
) -> Result<StreamSink, StartInfoProgramError> {
    match runner::get_enum(dict, key, &["log", "none"])? {
        Some("log") => Ok(StreamSink::Log),
        Some("none") => Ok(StreamSink::None),
        Some(_) => unreachable!("get_enum returns only values in variants"),
        None => Ok(StreamSink::default()),
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        ::routing::{
            config::{
                AllowlistEntryBuilder, ChildPolicyAllowlists, JobPolicyAllowlists, SecurityPolicy,
            },
            policy::{PolicyError, ScopedPolicyChecker},
        },
        assert_matches::assert_matches,
        fidl_fuchsia_data as fdata,
        lazy_static::lazy_static,
        moniker::{Moniker, MonikerBase},
        std::{collections::HashMap, default::Default, sync::Arc},
        test_case::test_case,
    };

    const BINARY_KEY: &str = "binary";
    const TEST_BINARY: &str = "test_binary";

    lazy_static! {
        static ref TEST_MONIKER: Moniker = Moniker::root();
        static ref PERMISSIVE_SECURITY_POLICY: Arc<SecurityPolicy> = Arc::new(SecurityPolicy {
            job_policy: JobPolicyAllowlists {
                ambient_mark_vmo_exec: vec![AllowlistEntryBuilder::build_exact_from_moniker(
                    &TEST_MONIKER
                ),],
                main_process_critical: vec![AllowlistEntryBuilder::build_exact_from_moniker(
                    &TEST_MONIKER
                ),],
                create_raw_processes: vec![AllowlistEntryBuilder::build_exact_from_moniker(
                    &TEST_MONIKER
                ),],
            },
            capability_policy: HashMap::new(),
            debug_capability_policy: HashMap::new(),
            child_policy: ChildPolicyAllowlists { reboot_on_terminate: vec![] },
        });
        static ref RESTRICTIVE_SECURITY_POLICY: Arc<SecurityPolicy> =
            Arc::new(SecurityPolicy::default());
    }

    macro_rules! assert_error_is_invalid_value {
        ($result:expr, $expected_key:expr) => {
            assert_matches!(
                $result,
                Err(ProgramError::Parse(StartInfoProgramError::InvalidValue(key, _, _)))
                if key == $expected_key
            );
        };
    }

    macro_rules! assert_error_is_invalid_type {
        ($result:expr, $expected_key:expr) => {
            assert_matches!(
                $result,
                Err(ProgramError::Parse(StartInfoProgramError::InvalidType(key)))
                if key == $expected_key
            );
        };
    }

    macro_rules! assert_error_is_disallowed_job_policy {
        ($result:expr, $expected_policy:expr) => {
            assert_matches!(
                $result,
                Err(ProgramError::Policy(PolicyError::JobPolicyDisallowed {
                    policy,
                    ..
                }))
                if policy == $expected_policy
            );
        };
    }

    #[test_case("forward_stdout_to", new_string("log"), ElfProgramConfig { stdout_sink: StreamSink::Log, ..default_valid_config()} ; "when_stdout_log")]
    #[test_case("forward_stdout_to", new_string("none"), ElfProgramConfig { stdout_sink: StreamSink::None, ..default_valid_config()} ; "when_stdout_none")]
    #[test_case("forward_stderr_to", new_string("log"), ElfProgramConfig { stderr_sink: StreamSink::Log, ..default_valid_config()} ; "when_stderr_log")]
    #[test_case("forward_stderr_to", new_string("none"), ElfProgramConfig { stderr_sink: StreamSink::None, ..default_valid_config()} ; "when_stderr_none")]
    #[test_case("environ", new_empty_vec(), ElfProgramConfig { environ: None, ..default_valid_config()} ; "when_environ_empty")]
    #[test_case("environ", new_vec(vec!["FOO=BAR"]), ElfProgramConfig { environ: Some(vec!["FOO=BAR".into()]), ..default_valid_config()} ; "when_environ_has_values")]
    #[test_case("lifecycle.stop_event", new_string("notify"), ElfProgramConfig { notify_lifecycle_stop: true, ..default_valid_config()} ; "when_stop_event_notify")]
    #[test_case("lifecycle.stop_event", new_string("ignore"), ElfProgramConfig { notify_lifecycle_stop: false, ..default_valid_config()} ; "when_stop_event_ignore")]
    #[test_case("main_process_critical", new_string("true"), ElfProgramConfig { main_process_critical: true, ..default_valid_config()} ; "when_main_process_critical_true")]
    #[test_case("main_process_critical", new_string("false"), ElfProgramConfig { main_process_critical: false, ..default_valid_config()} ; "when_main_process_critical_false")]
    #[test_case("job_policy_ambient_mark_vmo_exec", new_string("true"), ElfProgramConfig { ambient_mark_vmo_exec: true, ..default_valid_config()} ; "when_ambient_mark_vmo_exec_true")]
    #[test_case("job_policy_ambient_mark_vmo_exec", new_string("false"), ElfProgramConfig { ambient_mark_vmo_exec: false, ..default_valid_config()} ; "when_ambient_mark_vmo_exec_false")]
    #[test_case("job_policy_create_raw_processes", new_string("true"), ElfProgramConfig { create_raw_processes: true, ..default_valid_config()} ; "when_create_raw_processes_true")]
    #[test_case("job_policy_create_raw_processes", new_string("false"), ElfProgramConfig { create_raw_processes: false, ..default_valid_config()} ; "when_create_raw_processes_false")]
    #[test_case("use_next_vdso", new_string("true"), ElfProgramConfig { use_next_vdso: true, ..default_valid_config()} ; "use_next_vdso_true")]
    #[test_case("use_next_vdso", new_string("false"), ElfProgramConfig { use_next_vdso: false, ..default_valid_config()} ; "use_next_vdso_false")]
    fn test_parse_and_check_with_permissive_policy(
        key: &str,
        value: fdata::DictionaryValue,
        expected: ElfProgramConfig,
    ) {
        let checker =
            ScopedPolicyChecker::new(PERMISSIVE_SECURITY_POLICY.clone(), TEST_MONIKER.clone());
        let program = new_program_stanza(key, value);

        let actual = ElfProgramConfig::parse_and_check(&program, &checker).unwrap();

        assert_eq!(actual, expected);
    }

    #[test_case("job_policy_ambient_mark_vmo_exec", new_string("true") , "ambient_mark_vmo_exec" ; "when_ambient_mark_vmo_exec_true")]
    #[test_case("main_process_critical", new_string("true"), "main_process_critical" ; "when_main_process_critical_true")]
    #[test_case("job_policy_create_raw_processes", new_string("true"), "create_raw_processes" ; "when_create_raw_processes_true")]
    fn test_parse_and_check_with_restrictive_policy(
        key: &str,
        value: fdata::DictionaryValue,
        policy: &str,
    ) {
        let checker =
            ScopedPolicyChecker::new(RESTRICTIVE_SECURITY_POLICY.clone(), TEST_MONIKER.clone());
        let program = new_program_stanza(key, value);

        let actual = ElfProgramConfig::parse_and_check(&program, &checker);

        assert_error_is_disallowed_job_policy!(actual, policy);
    }

    #[test_case("lifecycle.stop_event", new_string("invalid") ; "for_stop_event")]
    #[test_case("environ", new_empty_string() ; "for_environ")]
    fn test_parse_and_check_with_invalid_value(key: &str, value: fdata::DictionaryValue) {
        // Use a permissive policy because we want to fail *iff* value set for
        // key is invalid.
        let checker =
            ScopedPolicyChecker::new(PERMISSIVE_SECURITY_POLICY.clone(), TEST_MONIKER.clone());
        let program = new_program_stanza(key, value);

        let actual = ElfProgramConfig::parse_and_check(&program, &checker);

        assert_error_is_invalid_value!(actual, key);
    }

    #[test_case("lifecycle.stop_event", new_empty_vec() ; "for_stop_event")]
    #[test_case("job_policy_ambient_mark_vmo_exec", new_empty_vec() ; "for_ambient_mark_vmo_exec")]
    #[test_case("main_process_critical", new_empty_vec() ; "for_main_process_critical")]
    #[test_case("job_policy_create_raw_processes", new_empty_vec() ; "for_create_raw_processes")]
    #[test_case("forward_stdout_to", new_empty_vec() ; "for_stdout")]
    #[test_case("forward_stderr_to", new_empty_vec() ; "for_stderr")]
    #[test_case("use_next_vdso", new_empty_vec() ; "for_use_next_vdso")]
    fn test_parse_and_check_with_invalid_type(key: &str, value: fdata::DictionaryValue) {
        // Use a permissive policy because we want to fail *iff* value set for
        // key is invalid.
        let checker =
            ScopedPolicyChecker::new(PERMISSIVE_SECURITY_POLICY.clone(), TEST_MONIKER.clone());
        let program = new_program_stanza(key, value);

        let actual = ElfProgramConfig::parse_and_check(&program, &checker);

        assert_error_is_invalid_type!(actual, key);
    }

    fn default_valid_config() -> ElfProgramConfig {
        ElfProgramConfig { binary: TEST_BINARY.to_string(), args: Vec::new(), ..Default::default() }
    }

    fn new_program_stanza(key: &str, value: fdata::DictionaryValue) -> fdata::Dictionary {
        fdata::Dictionary {
            entries: Some(vec![
                fdata::DictionaryEntry {
                    key: BINARY_KEY.to_owned(),
                    value: Some(Box::new(new_string(TEST_BINARY))),
                },
                fdata::DictionaryEntry { key: key.to_owned(), value: Some(Box::new(value)) },
            ]),
            ..Default::default()
        }
    }

    fn new_string(value: &str) -> fdata::DictionaryValue {
        fdata::DictionaryValue::Str(value.to_owned())
    }

    fn new_vec(values: Vec<&str>) -> fdata::DictionaryValue {
        fdata::DictionaryValue::StrVec(values.into_iter().map(str::to_owned).collect())
    }

    fn new_empty_string() -> fdata::DictionaryValue {
        fdata::DictionaryValue::Str("".to_owned())
    }

    fn new_empty_vec() -> fdata::DictionaryValue {
        fdata::DictionaryValue::StrVec(vec![])
    }
}
