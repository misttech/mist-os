// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{format_err, Result};
use at_commands as at;

use super::{at_cmd, at_ok, at_resp};
use super::{Procedure, ProcedureInput, ProcedureOutput};

use crate::features::{extract_features_from_command, AgFeatures, CVSD, MSBC};
use crate::peer::indicators::{BATTERY_LEVEL, ENHANCED_SAFETY, INDICATOR_REPORTING_MODE};
use crate::peer::procedure_manipulated_state::ProcedureManipulatedState;

#[derive(Debug, PartialEq)]
pub enum State {
    SentSupportedFeatures,         // Sent AT+BRSF, waiting for +BRSF.
    ReceivedSupportedFeatures,     // Received +BRSF, waiting for OK.
    SentAvailableCodecs,           // Sent AT+BAC, waiting for OK.
    TestedSupportedAgIndicators,   // Sent AT+CIND=?, waiting for first +CIND
    ReceivedSupportedAgIndicators, // Waiting for OK after +CIND
    ReadAgIndicatorStatuses,       // Sent AT+CIND?, waiting for second +CIND
    ReceivedAgIndicatorStatuses,   // Waiting for OK after second +CIND
    SentAgIndicatorStatusUpdate,   // Sent AT+CMER, waiting for OK.
    SentCallHoldAndMultiparty,     // Sent AT+CHLD, waiting for +CHLD
    ReceivedCallHoldAndMultiparty, // Waiting for OK after +CHLD
    SentSupportedHfIndicators,     // Sent AT+BIND, waiting for OK.
    TestedSupportedHfIndicators,   // Sent AT+BIND=?, waiting for +BIND.
    ReceivedSupportedHfIndicators, // Waiting for OK after +BIND.
    ReadEnabledHfIndicators,       // Sent AT+BIND?, waiting for +BIND or OK.
    Terminated,
}

#[derive(Debug)]
pub struct SlcInitProcedure {
    state: State,
}

impl SlcInitProcedure {
    pub fn new() -> Self {
        Self { state: State::SentSupportedFeatures }
    }

    #[cfg(test)]
    pub fn start_at_state(state: State) -> Self {
        Self { state, ..SlcInitProcedure::new() }
    }

    #[cfg(test)]
    pub fn start_terminated() -> Self {
        Self { state: State::Terminated }
    }

    fn receive_supported_features(
        &mut self,
        state: &mut ProcedureManipulatedState,
        features: i64,
    ) -> Option<ProcedureOutput> {
        state.ag_features = AgFeatures::from_bits_truncate(features);
        self.state = State::ReceivedSupportedFeatures;
        None
    }

    fn send_available_codecs(
        &mut self,
        state: &mut ProcedureManipulatedState,
    ) -> Option<ProcedureOutput> {
        self.state = State::SentAvailableCodecs;
        state.supported_codecs.push(MSBC);
        // TODO(https://fxbug.dev/42081215) Make this configurable.
        // By default, we support the CVSD and MSBC codecs.
        Some(at_cmd!(Bac { codecs: vec![CVSD.into(), MSBC.into()] }))
    }

    fn test_supported_ag_indicators(&mut self) -> Option<ProcedureOutput> {
        self.state = State::TestedSupportedAgIndicators;
        Some(at_cmd!(CindTest {}))
    }

    fn receive_supported_ag_indicators(&mut self, _bytes: Vec<u8>) -> Option<ProcedureOutput> {
        // TODO(https://fxbug.dev/42059744): Read additional indicators by parsing raw bytes instead
        // ofjust checking for existence of raw bytes.
        self.state = State::ReceivedSupportedAgIndicators;
        None
    }

    fn read_ag_indicator_statuses(&mut self) -> Option<ProcedureOutput> {
        self.state = State::ReadAgIndicatorStatuses;
        Some(at_cmd!(CindRead {}))
    }

    fn receive_ag_indicator_statuses(
        &mut self,
        state: &mut ProcedureManipulatedState,
        cmd: &at::Success,
    ) -> Option<ProcedureOutput> {
        state.ag_indicators.update_indicator_values(cmd);
        self.state = State::ReceivedAgIndicatorStatuses;
        None
    }

    fn send_ag_indicator_status_update(&mut self) -> Option<ProcedureOutput> {
        self.state = State::SentAgIndicatorStatusUpdate;
        Some(at_cmd!(Cmer { mode: INDICATOR_REPORTING_MODE, keyp: 0, disp: 0, ind: 1 }))
    }

    fn send_call_hold_and_multparty(&mut self) -> Option<ProcedureOutput> {
        self.state = State::SentCallHoldAndMultiparty;
        Some(at_cmd!(ChldTest {}))
    }

    fn receive_call_hold_and_multiparty(
        &mut self,
        state: &mut ProcedureManipulatedState,
        commands: &Vec<String>,
    ) -> Result<Option<ProcedureOutput>> {
        state.three_way_features = extract_features_from_command(commands)?;
        self.state = State::ReceivedCallHoldAndMultiparty;
        Ok(None)
    }

    fn send_supported_hf_indicators(&mut self) -> Option<ProcedureOutput> {
        self.state = State::SentSupportedHfIndicators;
        Some(at_cmd!(Bind { indicators: vec![ENHANCED_SAFETY as i64, BATTERY_LEVEL as i64] }))
    }

    fn test_supported_hf_indicators(&mut self) -> Option<ProcedureOutput> {
        self.state = State::TestedSupportedHfIndicators;
        Some(at_cmd!(BindTest {}))
    }

    fn receive_supported_hf_indicators(
        &mut self,
        state: &mut ProcedureManipulatedState,
        indicators: &Vec<at::BluetoothHFIndicator>,
    ) -> Option<ProcedureOutput> {
        state.hf_indicators.set_supported_indicators(indicators);
        self.state = State::ReceivedSupportedHfIndicators;
        None
    }

    fn read_enabled_hf_indicators(&mut self) -> Option<ProcedureOutput> {
        self.state = State::ReadEnabledHfIndicators;
        Some(at_cmd!(BindRead {}))
    }

    fn receive_enabled_hf_indicator(
        &mut self,
        state: &mut ProcedureManipulatedState,
        cmd: &at::Success,
    ) -> Result<Option<ProcedureOutput>> {
        state.hf_indicators.change_indicator_state(cmd)?;
        Ok(None)
    }

    fn terminate(&mut self) -> Option<ProcedureOutput> {
        self.state = State::Terminated;
        None
    }
}

impl Procedure<ProcedureInput, ProcedureOutput> for SlcInitProcedure {
    fn new() -> Self {
        Self { state: State::SentSupportedFeatures }
    }

    fn name(&self) -> &str {
        "SLC Initialization"
    }

    /// Checks for sequential ordering of commands by first checking the
    /// stage the SLCI is in and then extract important data from AG responses
    /// and proceed to next stage if necessary.
    fn transition(
        &mut self,
        state: &mut ProcedureManipulatedState,
        input: ProcedureInput,
    ) -> Result<Vec<ProcedureOutput>> {
        if self.is_terminated() {
            return Err(format_err!(
                "Procedure is already terminated before processing update for response(s): {:?}.",
                input
            ));
        }
        let output_option = match (&self.state, input) {
            // Sent AT+BRSF, waiting for +BRSF /////////////////////////////////////////////////////
            (State::SentSupportedFeatures, at_resp!(Brsf { features })) => {
                self.receive_supported_features(state, features)
            }

            // Received +BSRF, waiting for OK //////////////////////////////////////////////////////
            (State::ReceivedSupportedFeatures, at_ok!()) => {
                if state.supports_codec_negotiation() {
                    self.send_available_codecs(state)
                } else {
                    self.test_supported_ag_indicators()
                }
            }

            // Sent AT+BAC, waiting for OK /////////////////////////////////////////////////////////
            (State::SentAvailableCodecs, at_ok!()) => self.test_supported_ag_indicators(),

            // Sent AT+CIND=?, waiting for +CIND: //////////////////////////////////////////////////
            (
                State::TestedSupportedAgIndicators,
                ProcedureInput::AtResponseFromAg(at::Response::RawBytes(bytes)),
            ) => self.receive_supported_ag_indicators(bytes),

            // Received +CIND, waiting for OK /////////////////////////////////////////////////////
            (State::ReceivedSupportedAgIndicators, at_ok!()) => self.read_ag_indicator_statuses(),

            // Sent AT+CIND?, waiting for +CIND: ///////////////////////////////////////////////////
            (
                State::ReadAgIndicatorStatuses,
                ProcedureInput::AtResponseFromAg(at::Response::Success(
                    cmd @ at::Success::Cind { .. },
                )),
            ) => self.receive_ag_indicator_statuses(state, &cmd),

            // Received +CIND:, waiting for OK /////////////////////////////////////////////////////
            (State::ReceivedAgIndicatorStatuses, at_ok!()) => {
                self.send_ag_indicator_status_update()
            }

            // Sent AT+CMER, waiting for OK ////////////////////////////////////////////////////////
            (State::SentAgIndicatorStatusUpdate, at_ok!()) => {
                if state.supports_three_way_calling() {
                    self.send_call_hold_and_multparty()
                } else if state.supports_hf_indicators() {
                    self.send_supported_hf_indicators()
                } else {
                    self.terminate()
                }
            }

            // Sent AT+CHLD=?, waiting for +CHLD: //////////////////////////////////////////////////
            (State::SentCallHoldAndMultiparty, at_resp!(Chld { commands })) => {
                self.receive_call_hold_and_multiparty(state, &commands)?
            }

            // Received +CHLD:, waiting for OK /////////////////////////////////////////////////////
            (State::ReceivedCallHoldAndMultiparty, at_ok!()) => {
                if state.supports_hf_indicators() {
                    self.send_supported_hf_indicators()
                } else {
                    self.terminate()
                }
            }
            // Sent AT+BIND=, waiting for OK ///////////////////////////////////////////////////////
            (State::SentSupportedHfIndicators, at_ok!()) => self.test_supported_hf_indicators(),

            // Sent AT+BIND=?, waiting for +BIND: ///////////////////////////////////////////////////
            (State::TestedSupportedHfIndicators, at_resp!(BindList { indicators })) => {
                self.receive_supported_hf_indicators(state, &indicators)
            }

            // Received +BIND:, waiting for OK /////////////////////////////////////////////////////
            (State::ReceivedSupportedHfIndicators, at_ok!()) => self.read_enabled_hf_indicators(),

            // Sent AT+BIND?, waiting for +BIND: or OK /////////////////////////////////////////////
            (
                State::ReadEnabledHfIndicators,
                ProcedureInput::AtResponseFromAg(at::Response::Success(
                    cmd @ at::Success::BindStatus { .. },
                )),
            ) => self.receive_enabled_hf_indicator(state, &cmd)?,

            // Sent AT+BIND?, waiting for +BIND: or OK /////////////////////////////////////////////
            (State::ReadEnabledHfIndicators, at_ok!()) => self.terminate(),

            // Unexpected AT response for state ////////////////////////////////////////////////////
            (state, input) => {
                // Early return for error
                return Err(format_err!(
                    "Wrong responses at {:?} stage of SLCI with response(s): {:?}.",
                    state,
                    input
                ));
            }
        };

        let outputs = output_option.into_iter().collect();
        Ok(outputs)
    }

    fn is_terminated(&self) -> bool {
        self.state == State::Terminated
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::{config::HandsFreeFeatureSupport, features::CallHoldAction, features::HfFeatures};
    use assert_matches::assert_matches;

    // TODO(https://fxbug.dev/42151045) Stop using raw bytes.
    const CIND_TEST_RESPONSE_BYTES: &[u8] = b"+CIND: \
    (\"service\",(0,1)),\
    (\"call\",(0,1)),\
    (\"callsetup\",(0,3)),\
    (\"callheld\",(0,2)),\
    (\"signal\",(0,5)),\
    (\"roam\",(0,1)),\
    (\"battchg\",(0,5)\
    )";

    #[fuchsia::test]
    /// Checks that the mandatory exchanges between the AG and HF roles properly progresses
    /// our state and sends the expected responses until our procedure it marked complete.
    fn slci_mandatory_exchanges_and_termination() {
        let mut procedure = SlcInitProcedure::new();
        let config = HandsFreeFeatureSupport::default();
        let mut state = ProcedureManipulatedState::new(config);

        assert!(!procedure.is_terminated());

        let response1 = at_resp!(Brsf { features: AgFeatures::default().bits() });
        let response1_ok = at_ok!();
        let expected_command1 = vec![at_cmd!(CindTest {})];

        assert_eq!(procedure.transition(&mut state, response1).unwrap(), vec![]);
        assert_eq!(procedure.transition(&mut state, response1_ok).unwrap(), expected_command1);

        let indicator_msg = CIND_TEST_RESPONSE_BYTES.to_vec();
        let response2 = ProcedureInput::AtResponseFromAg(at::Response::RawBytes(indicator_msg));
        let response2_ok = at_ok!();
        let expected_command2 = vec![at_cmd!(CindRead {})];
        assert_eq!(procedure.transition(&mut state, response2).unwrap(), vec![]);

        assert_eq!(procedure.transition(&mut state, response2_ok).unwrap(), expected_command2);

        let response3 = at_resp!(Cind {
            service: false,
            call: false,
            callsetup: 0,
            callheld: 0,
            signal: 0,
            roam: false,
            battchg: 0,
        });
        let response3_ok = at_ok!();
        let update3 =
            vec![at_cmd!(Cmer { mode: INDICATOR_REPORTING_MODE, keyp: 0, disp: 0, ind: 1 })];
        assert_eq!(procedure.transition(&mut state, response3).unwrap(), vec![]);
        assert_eq!(procedure.transition(&mut state, response3_ok).unwrap(), update3);

        let response4 = at_ok!();
        assert_eq!(procedure.transition(&mut state, response4).unwrap(), vec![]);

        assert!(procedure.is_terminated());
    }

    #[fuchsia::test]
    fn slci_hf_indicator_properly_works() {
        let mut procedure = SlcInitProcedure::new();
        // Hf indicators needed for optional procedure.
        let mut hf_features = HfFeatures::default();
        let mut ag_features = AgFeatures::default();
        hf_features.set(HfFeatures::HF_INDICATORS, true);
        ag_features.set(AgFeatures::HF_INDICATORS, true);
        let mut state = ProcedureManipulatedState::load_with_set_features(hf_features, ag_features);

        assert!(!state.hf_indicators.enhanced_safety.1);
        assert!(!state.hf_indicators.battery_level.1);
        assert!(!state.hf_indicators.enhanced_safety.0.enabled);
        assert!(!state.hf_indicators.battery_level.0.enabled);
        assert!(!procedure.is_terminated());

        let response1 = at_resp!(Brsf { features: ag_features.bits() });
        let response1_ok = at_ok!();
        let expected_command1 = vec![at_cmd!(CindTest {})];

        assert_eq!(procedure.transition(&mut state, response1).unwrap(), vec![]);
        assert_eq!(procedure.transition(&mut state, response1_ok).unwrap(), expected_command1);

        let indicator_msg = CIND_TEST_RESPONSE_BYTES.to_vec();
        let response2 = ProcedureInput::AtResponseFromAg(at::Response::RawBytes(indicator_msg));
        let response2_ok = at_ok!();
        let expected_command2 = vec![at_cmd!(CindRead {})];

        assert_eq!(procedure.transition(&mut state, response2).unwrap(), vec![]);
        assert_eq!(procedure.transition(&mut state, response2_ok).unwrap(), expected_command2);

        let response3 = at_resp!(Cind {
            service: false,
            call: false,
            callsetup: 0,
            callheld: 0,
            signal: 0,
            roam: false,
            battchg: 0,
        });
        let response3_ok = at_ok!();
        let expected_command3 =
            vec![at_cmd!(Cmer { mode: INDICATOR_REPORTING_MODE, keyp: 0, disp: 0, ind: 1 })];

        assert_eq!(procedure.transition(&mut state, response3).unwrap(), vec![]);
        assert_eq!(procedure.transition(&mut state, response3_ok).unwrap(), expected_command3);

        let response4 = at_ok!();
        let expected_command4 =
            vec![at_cmd!(Bind { indicators: vec![ENHANCED_SAFETY as i64, BATTERY_LEVEL as i64] })];
        assert_eq!(procedure.transition(&mut state, response4).unwrap(), expected_command4);

        let response5 = at_ok!();
        let expected_command5 = vec![at_cmd!(BindTest {})];
        assert_eq!(procedure.transition(&mut state, response5).unwrap(), expected_command5);

        let response6 = at_resp!(BindList {
            indicators: vec![
                at::BluetoothHFIndicator::BatteryLevel,
                at::BluetoothHFIndicator::EnhancedSafety,
            ],
        });
        let response6_ok = at_ok!();
        let expected_command6 = vec![at_cmd!(BindRead {})];
        assert_eq!(procedure.transition(&mut state, response6).unwrap(), vec![]);
        assert_eq!(procedure.transition(&mut state, response6_ok).unwrap(), expected_command6);
        assert!(state.hf_indicators.enhanced_safety.1);
        assert!(state.hf_indicators.battery_level.1);

        let response7 =
            at_resp!(BindStatus { anum: at::BluetoothHFIndicator::EnhancedSafety, state: true });
        let response8 =
            at_resp!(BindStatus { anum: at::BluetoothHFIndicator::BatteryLevel, state: true });
        let response9 = at_ok!();
        assert_eq!(procedure.transition(&mut state, response7).unwrap(), vec![]);
        assert_eq!(procedure.transition(&mut state, response8).unwrap(), vec![]);
        assert_eq!(procedure.transition(&mut state, response9).unwrap(), vec![]);
        assert!(state.hf_indicators.enhanced_safety.0.enabled);
        assert!(state.hf_indicators.battery_level.0.enabled);
    }

    #[fuchsia::test]
    fn slci_codec_negotiation_properly_works() {
        let mut procedure = SlcInitProcedure::new();
        let mut hf_features = HfFeatures::default();
        hf_features.set(HfFeatures::CODEC_NEGOTIATION, true);
        let mut ag_features = AgFeatures::default();
        ag_features.set(AgFeatures::CODEC_NEGOTIATION, true);
        let mut state = ProcedureManipulatedState::load_with_set_features(hf_features, ag_features);

        assert!(!procedure.is_terminated());

        let response1 = at_resp!(Brsf { features: ag_features.bits() });
        let response1_ok = at_ok!();
        let expected_command1 = vec![at_cmd!(Bac { codecs: vec![CVSD.into(), MSBC.into()] })];

        assert_eq!(procedure.transition(&mut state, response1).unwrap(), vec![]);
        assert_eq!(procedure.transition(&mut state, response1_ok).unwrap(), expected_command1);

        let response2 = at_ok!();
        let expected_command2 = vec![at_cmd!(CindTest {})];

        assert_eq!(procedure.transition(&mut state, response2).unwrap(), expected_command2);
        assert!(!procedure.is_terminated());
    }

    #[fuchsia::test]
    fn slci_three_way_feature_proper_works() {
        let mut procedure = SlcInitProcedure::new();
        // Three way calling needed for stage progression.
        let mut hf_features = HfFeatures::default();
        hf_features.set(HfFeatures::THREE_WAY_CALLING, true);
        let mut ag_features = AgFeatures::default();
        ag_features.set(AgFeatures::THREE_WAY_CALLING, true);
        let mut state = ProcedureManipulatedState::load_with_set_features(hf_features, ag_features);

        assert!(!procedure.is_terminated());

        let response1 = at_resp!(Brsf { features: ag_features.bits() });
        let response1_ok = at_ok!();
        let expected_command1 = vec![at_cmd!(CindTest {})];

        assert_eq!(procedure.transition(&mut state, response1).unwrap(), vec![]);
        assert_eq!(procedure.transition(&mut state, response1_ok).unwrap(), expected_command1);

        let indicator_msg = CIND_TEST_RESPONSE_BYTES.to_vec();
        let response2 = ProcedureInput::AtResponseFromAg(at::Response::RawBytes(indicator_msg));
        let response2_ok = at_ok!();
        let expected_command2 = vec![at_cmd!(CindRead {})];
        assert_eq!(procedure.transition(&mut state, response2).unwrap(), vec![]);
        assert_eq!(procedure.transition(&mut state, response2_ok).unwrap(), expected_command2);

        let response3 = at_resp!(Cind {
            service: false,
            call: false,
            callsetup: 0,
            callheld: 0,
            signal: 0,
            roam: false,
            battchg: 0,
        });
        let response3_ok = at_ok!();
        let update3 =
            vec![at_cmd!(Cmer { mode: INDICATOR_REPORTING_MODE, keyp: 0, disp: 0, ind: 1 })];
        assert_eq!(procedure.transition(&mut state, response3).unwrap(), vec![]);
        assert_eq!(procedure.transition(&mut state, response3_ok).unwrap(), update3);

        let response4 = at_ok!();
        let update4 = vec![at_cmd!(ChldTest {})];
        assert_eq!(procedure.transition(&mut state, response4).unwrap(), update4);

        let commands = vec![
            String::from("0"),
            String::from("1"),
            String::from("2"),
            String::from("11"),
            String::from("22"),
            String::from("3"),
            String::from("4"),
        ];
        let response5 = at_resp!(Chld { commands });
        let response5_ok = at_ok!();
        assert_eq!(procedure.transition(&mut state, response5).unwrap(), vec![]);
        assert_eq!(procedure.transition(&mut state, response5_ok).unwrap(), vec![]);
        assert!(procedure.is_terminated());

        let features = vec![
            CallHoldAction::ReleaseAllHeld,
            CallHoldAction::ReleaseAllActive,
            CallHoldAction::HoldActiveAndAccept,
            CallHoldAction::ReleaseSpecified(1),
            CallHoldAction::HoldAllExceptSpecified(2),
            CallHoldAction::AddCallToHeldConversation,
            CallHoldAction::ExplicitCallTransfer,
        ];

        assert_eq!(features, state.three_way_features);
    }

    #[fuchsia::test]
    fn error_when_incorrect_response_at_feature_stage() {
        let mut procedure = SlcInitProcedure::start_at_state(State::SentSupportedFeatures);
        let config = HandsFreeFeatureSupport::default();
        let mut state = ProcedureManipulatedState::new(config);

        assert!(!procedure.is_terminated());

        let wrong_response = at_resp!(TestResponse {});
        assert_matches!(procedure.transition(&mut state, wrong_response), Err(_));
        assert!(!procedure.is_terminated());
    }

    #[fuchsia::test]
    fn error_when_incorrect_response_at_codec_negotiation_stage() {
        let mut procedure = SlcInitProcedure::start_at_state(State::SentAvailableCodecs);
        let mut hf_features = HfFeatures::default();
        hf_features.set(HfFeatures::CODEC_NEGOTIATION, true);
        let mut ag_features = AgFeatures::default();
        ag_features.set(AgFeatures::CODEC_NEGOTIATION, true);
        let mut state = ProcedureManipulatedState::load_with_set_features(hf_features, ag_features);

        assert!(!procedure.is_terminated());

        let wrong_response = at_resp!(TestResponse {});
        assert_matches!(procedure.transition(&mut state, wrong_response), Err(_));
        assert!(!procedure.is_terminated());
    }

    #[fuchsia::test]
    fn error_when_incorrect_response_at_list_indicators_stage() {
        let mut procedure = SlcInitProcedure::start_at_state(State::TestedSupportedAgIndicators);
        let config = HandsFreeFeatureSupport::default();
        let mut state = ProcedureManipulatedState::new(config);

        assert!(!procedure.is_terminated());

        let wrong_response = at_resp!(TestResponse {});
        assert_matches!(procedure.transition(&mut state, wrong_response), Err(_));
        assert!(!procedure.is_terminated());
    }

    #[fuchsia::test]
    fn error_when_incorrect_response_at_enable_indicators_stage() {
        let mut procedure = SlcInitProcedure::start_at_state(State::ReceivedAgIndicatorStatuses);
        let config = HandsFreeFeatureSupport::default();
        let mut state = ProcedureManipulatedState::new(config);

        assert!(!procedure.is_terminated());

        let wrong_response = at_resp!(TestResponse {});
        assert_matches!(procedure.transition(&mut state, wrong_response), Err(_));
        assert!(!procedure.is_terminated());
    }

    #[fuchsia::test]
    fn error_when_incorrect_response_at_indicator_update_stage() {
        let mut procedure = SlcInitProcedure::start_at_state(State::SentAgIndicatorStatusUpdate);
        let config = HandsFreeFeatureSupport::default();
        let mut state = ProcedureManipulatedState::new(config);

        assert!(!procedure.is_terminated());

        let wrong_response = at_resp!(TestResponse {});
        assert_matches!(procedure.transition(&mut state, wrong_response), Err(_));
        assert!(!procedure.is_terminated());
    }

    #[fuchsia::test]
    fn error_when_incorrect_response_at_call_hold_stage_non_number_index() {
        let mut procedure = SlcInitProcedure::start_at_state(State::SentCallHoldAndMultiparty);
        let config = HandsFreeFeatureSupport {
            call_waiting_or_three_way_calling: true,
            ..HandsFreeFeatureSupport::default()
        };
        let mut state = ProcedureManipulatedState::new(config);

        assert!(!procedure.is_terminated());

        let invalid_command = vec![String::from("1A")];
        let response = at_resp!(Chld { commands: invalid_command });

        assert_matches!(procedure.transition(&mut state, response), Err(_));
        assert!(!procedure.is_terminated());
    }

    #[fuchsia::test]
    fn error_when_incorrect_response_at_call_hold_stage_invalid_command() {
        let mut procedure = SlcInitProcedure::start_at_state(State::SentCallHoldAndMultiparty);
        let config = HandsFreeFeatureSupport {
            call_waiting_or_three_way_calling: true,
            ..HandsFreeFeatureSupport::default()
        };
        let mut state = ProcedureManipulatedState::new(config);

        assert!(!procedure.is_terminated());

        let invalid_command = vec![String::from("5")];
        let response = at_resp!(Chld { commands: invalid_command });

        assert_matches!(procedure.transition(&mut state, response), Err(_));
        assert!(!procedure.is_terminated());
    }

    #[fuchsia::test]
    fn error_when_incorrect_response_at_hf_indicator_stage() {
        let mut procedure = SlcInitProcedure::start_at_state(State::SentSupportedHfIndicators);
        let config = HandsFreeFeatureSupport::default();
        let mut state = ProcedureManipulatedState::new(config);

        assert!(!procedure.is_terminated());

        // Did not receive expected Ok response as should result in error.
        let wrong_response = at_resp!(TestResponse {});
        assert_matches!(procedure.transition(&mut state, wrong_response), Err(_));
        assert!(!procedure.is_terminated());
    }

    #[fuchsia::test]
    fn error_when_incorrect_response_at_hf_indicator_request_stage() {
        let mut procedure = SlcInitProcedure::start_at_state(State::TestedSupportedHfIndicators);
        let config = HandsFreeFeatureSupport::default();
        let mut state = ProcedureManipulatedState::new(config);

        assert!(!procedure.is_terminated());

        // Did not receive expected Ok response as should result in error.
        let wrong_response = at_resp!(TestResponse {});
        assert_matches!(procedure.transition(&mut state, wrong_response), Err(_));
        assert!(!procedure.is_terminated());
    }

    #[fuchsia::test]
    fn error_when_incorrect_response_at_hf_indicator_enable_stage() {
        let mut procedure = SlcInitProcedure::start_at_state(State::ReadEnabledHfIndicators);
        let config = HandsFreeFeatureSupport::default();
        let mut state = ProcedureManipulatedState::new(config);

        assert!(!procedure.is_terminated());

        // Did not receive expected Ok response as should result in error.
        let wrong_response = at_resp!(TestResponse {});
        assert_matches!(procedure.transition(&mut state, wrong_response), Err(_));
        assert!(!procedure.is_terminated());
    }

    #[fuchsia::test]
    fn error_when_no_ok_at_hf_indicator_enable_stage() {
        let mut procedure = SlcInitProcedure::start_at_state(State::ReadEnabledHfIndicators);
        let config = HandsFreeFeatureSupport::default();
        let mut state = ProcedureManipulatedState::new(config);

        assert!(!procedure.is_terminated());

        let wrong_response = at_resp!(TestResponse {});
        assert_matches!(procedure.transition(&mut state, wrong_response), Err(_));
        assert!(!procedure.is_terminated());
    }

    #[fuchsia::test]
    fn error_when_update_on_terminated_procedure() {
        let mut procedure = SlcInitProcedure::start_terminated();
        let config = HandsFreeFeatureSupport::default();
        let mut state = ProcedureManipulatedState::new(config);

        assert!(procedure.is_terminated());
        // Valid response of first step of SLCI
        let valid_response = at_resp!(Brsf { features: AgFeatures::default().bits() });
        let update = procedure.transition(&mut state, valid_response);
        assert_matches!(update, Err(_));
    }
}
