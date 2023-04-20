// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package ffx

import (
	"context"
	"encoding/json"
	"fmt"
	"os/exec"

	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/util"
)

type FFXTool struct {
	ffxToolPath string
}

func NewFFXTool(ffxToolPath string) *FFXTool {
	return &FFXTool{
		ffxToolPath: ffxToolPath,
	}
}

type targetEntry struct {
	NodeName    string   `json:"nodename"`
	Addresses   []string `json:"addresses"`
	TargetState string   `json:"target_state"`
}

func (f *FFXTool) TargetList(ctx context.Context) ([]targetEntry, error) {
	args := []string{
		"--machine",
		"json",
		"target",
		"list",
	}

	stdout, stderr, err := util.RunCommand(ctx, f.ffxToolPath, args...)
	if err != nil {
		return []targetEntry{}, fmt.Errorf("ffx target list failed: %w: %s", err, string(stderr))
	}

	if len(stdout) == 0 {
		return []targetEntry{}, nil
	}

	var entries []targetEntry
	if err := json.Unmarshal(stdout, &entries); err != nil {
		return []targetEntry{}, err
	}

	return entries, nil
}

func (f *FFXTool) TargetListForNode(ctx context.Context, nodeNames []string) ([]targetEntry, error) {
	entries, err := f.TargetList(ctx)
	if err != nil {
		return []targetEntry{}, err
	}

	if len(nodeNames) == 0 {
		return entries, nil
	}

	var matchingTargets []targetEntry

	for _, target := range entries {
		for _, nodeName := range nodeNames {
			if target.NodeName == nodeName {
				matchingTargets = append(matchingTargets, target)
			}
		}
	}

	return matchingTargets, nil
}

func (f *FFXTool) SupportsZedbootDiscovery(ctx context.Context) (bool, error) {
	// Check if ffx is configured to resolve devices in zedboot.
	args := []string{
		"config",
		"get",
		"discovery.zedboot.enabled",
	}

	stdout, stderr, err := util.RunCommand(ctx, f.ffxToolPath, args...)
	if err != nil {
		// `ffx config get` exits with 2 if variable is undefined.
		if exiterr, ok := err.(*exec.ExitError); ok {
			if exiterr.ExitCode() == 2 {
				return false, nil
			}
		}

		return false, fmt.Errorf("ffx config get failed: %w: %s", err, string(stderr))
	}

	// FIXME(fxbug.dev/109280): Unfortunately we need to parse the raw string to see if it's true.
	if string(stdout) == "true\n" {
		return true, nil
	}

	return false, nil
}
