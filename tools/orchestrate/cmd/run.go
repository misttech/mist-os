// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
package main

import (
	"context"
	"flag"
	"fmt"

	"github.com/google/subcommands"
	"go.fuchsia.dev/fuchsia/tools/orchestrate"
)

type runCmd struct {
	input        string
	deviceConfig string
}

func (*runCmd) Name() string {
	return "run"
}

func (*runCmd) Synopsis() string {
	return "Runs fuchsia test in out-of-tree environment"
}

func (*runCmd) Usage() string {
	return "Usage: ./orchestrate run [flags...]"
}

func (r *runCmd) SetFlags(f *flag.FlagSet) {
	f.StringVar(&r.deviceConfig, "device-config", "/etc/botanist/config.json", "File path for device config JSON file.")
	f.StringVar(&r.input, "input", "", "File path for input JSON file.")
}

func (r *runCmd) Execute(_ context.Context, f *flag.FlagSet, _ ...any) subcommands.ExitStatus {
	oc := orchestrate.NewOrchestrateConfig()
	_, err := oc.ReadDeviceConfig(r.deviceConfig)
	if err != nil {
		fmt.Printf("Failed to read Device Config: %v\n", err)
		return subcommands.ExitFailure
	}
	_, err = oc.ReadRunInput(r.input)
	if err != nil {
		fmt.Printf("Reading run input failed: %v\n", err)
		return subcommands.ExitFailure
	}
	return subcommands.ExitSuccess
}
