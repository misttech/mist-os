// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package orchestrate

import (
	"encoding/json"
	"fmt"
	"os"
)

type OrchestrateConfig struct {
	ReadFile func(name string) ([]byte, error)
}

func NewOrchestrateConfig() *OrchestrateConfig {
	return &OrchestrateConfig{
		ReadFile: os.ReadFile,
	}
}

// DeviceNetworkConfig holds data for the network in /etc/botanist/config.json
type DeviceNetworkConfig struct {
	IPv4 string `json:"ipv4"`
}

// DeviceConfig holds all the data that we expect to be present in /etc/botanist/config.json
type DeviceConfig struct {
	FastbootSerial string              `json:"fastboot_sernum"`
	SerialMux      string              `json:"serial_mux"`
	Network        DeviceNetworkConfig `json:"network"`
}

// ReadDeviceConfig returns a DeviceConfig.
func (oc *OrchestrateConfig) ReadDeviceConfig(path string) (*DeviceConfig, error) {
	content, err := oc.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("readFile: %w", err)
	}
	var data []DeviceConfig
	if err = json.Unmarshal(content, &data); err != nil {
		return nil, fmt.Errorf("unmarshal: %w", err)
	}
	if len(data) != 1 {
		return nil, fmt.Errorf("botanist config: Want 1 device, got %d", len(data))
	}
	return &data[0], nil
}

// TargetRunInput is the struct that defines how to run a test in a hw or emulator target.
type TargetRunInput struct {
	// Product bundle URL to download with "ffx product download".
	TransferURL string `json:"transfer_url"`
	// Local product bundle path, either absolute or relative to CWD.
	LocalPB string `json:"local_pb"`
	// Fuchsia packages archives to publish with "ffx repository publish".
	PackageArchives []string `json:"package_archives"`
	// Build IDs to symbolize with "ffx debug symbol-index add".
	BuildIds []string `json:"build_ids"`
	// ffx binary path.
	FfxPath string `json:"ffx_path"`
	// Map of CIPD destination to CIPD package path:version.
	Cipd map[string]string `json:"cipd"`
	// Path to ffxluciauth for downloading internal product bundles.
	FfxluciauthPath string `json:"ffxluciauth_path"`
}

// HostRunInput is the struct that defines how to run a test without device provisioning.
type HostRunInput struct {
	// Map of CIPD destination to CIPD package path:version.
	Cipd map[string]string `json:"cipd"`
}

// RunInput is the struct that defines how to run a test.
type RunInput struct {
	// Global experiments enabled for this run.
	ExperimentSlice []string `json:"experiments"`
	// Configs specific for hardware.
	Hardware TargetRunInput `json:"hardware"`
	// Configs specific for emulator.
	Emulator TargetRunInput `json:"emulator"`
	// Configs specific for host.
	Host HostRunInput `json:"host"`
}

// Returns whether RunInput.Hardware was specified.
func (ri *RunInput) IsHardware() bool {
	return ri.Hardware.FfxPath != ""
}

// Returns whether RunInput.Emulator was specified.
func (ri *RunInput) IsEmulator() bool {
	return ri.Emulator.FfxPath != ""
}

// Returns whether RunInput.Hardware or RunInput.Emulator was specified.
func (ri *RunInput) IsTarget() bool {
	return ri.IsHardware() || ri.IsEmulator()
}

// Returns whether RunInput.Host was specified.
func (ri *RunInput) IsHost() bool {
	return !ri.IsTarget()
}

// Returns RunInput.Hardware if RunInput.IsHardware, otherwise RunInput.Emulator.
func (ri *RunInput) Target() TargetRunInput {
	if ri.IsHardware() {
		return ri.Hardware
	} else {
		return ri.Emulator
	}
}

// Returns Cipd from either Hardware, Emulator, or Host; whichever is specified.
func (ri *RunInput) Cipd() map[string]string {
	if ri.IsTarget() {
		return ri.Target().Cipd
	} else {
		return ri.Host.Cipd
	}
}

func (ri *RunInput) HasExperiment(experiment string) bool {
	for _, given_experiment := range ri.ExperimentSlice {
		if given_experiment == experiment {
			return true
		}
	}
	return false
}

// ReadRunInput reads RunInput from a given path.
func (oc *OrchestrateConfig) ReadRunInput(path string) (*RunInput, error) {
	content, err := oc.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("ReadFile: %w", err)
	}
	var data RunInput
	if err = json.Unmarshal(content, &data); err != nil {
		return nil, fmt.Errorf("Unmarshal: %w", err)
	}
	if data.IsTarget() && (data.Target().TransferURL == "") == (data.Target().LocalPB == "") {
		return nil, fmt.Errorf(
			"transfer_url = %q and local_pb = %q are mutually exclusive",
			data.Target().TransferURL,
			data.Target().LocalPB,
		)
	}
	return &data, nil
}
