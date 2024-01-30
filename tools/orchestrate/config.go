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

// HardwareRunInput is the struct that defines how to run a test in a hw target.
type HardwareRunInput struct {
	// Product bundle URL to download with "ffx product download".
	TransferURL string `json:"transfer_url"`
	// Fuchsia packages archives to publish with "ffx repository publish".
	PackageArchives []string `json:"package_archives"`
	// Build IDs to symbolize with "ffx debug symbol-index add"
	BuildIds []string `json:"build_ids"`
	// ffx binary path
	FfxPath string `json:"ffx_path"`
	// Map of CIPD destination to CIPD package path:version.
	Cipd map[string]string `json:"cipd"`
	// Path to ffxluciauth for downloading internal product bundles.
	FfxluciauthPath string `json:"ffxluciauth_path"`
}

// RunInput is the struct that defines how to run a test.
type RunInput struct {
	// Configs specific for hardware
	Hardware HardwareRunInput `json:"hardware"`
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
	return &data, nil
}
