// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package ffx

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"

	"go.fuchsia.dev/fuchsia/tools/lib/logger"
)

type IsolateDir struct {
	path string
}

func NewIsolateDir(path string) IsolateDir {
	return IsolateDir{path: path}
}

type FFXTool struct {
	ffxToolPath string
	isolateDir  IsolateDir
}

func NewFFXTool(ffxToolPath string, isolateDir IsolateDir) (*FFXTool, error) {
	if _, err := os.Stat(ffxToolPath); err != nil {
		return nil, fmt.Errorf("error accessing %v: %w", ffxToolPath, err)
	}

	return &FFXTool{
		ffxToolPath: ffxToolPath,
		isolateDir:  isolateDir,
	}, nil
}

func (f *FFXTool) IsolateDir() IsolateDir {
	return f.isolateDir
}

func (f *FFXTool) StopDaemon(ctx context.Context) error {
	args := []string{"daemon", "stop", "--wait"}
	_, err := f.runFFXCmd(ctx, args...)
	return err
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

	stdout, err := f.runFFXCmd(ctx, args...)
	if err != nil {
		return []targetEntry{}, fmt.Errorf("ffx target list failed: %w", err)
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

func (f *FFXTool) TargetListForNode(ctx context.Context, nodeName string) ([]targetEntry, error) {
	entries, err := f.TargetList(ctx)
	if err != nil {
		return []targetEntry{}, err
	}

	var matchingTargets []targetEntry

	for _, target := range entries {
		if target.NodeName == nodeName {
			matchingTargets = append(matchingTargets, target)
		}
	}

	return matchingTargets, nil
}

func (f *FFXTool) TargetGetSshAddress(ctx context.Context, target string) (string, error) {
	args := []string{
		"--target",
		target,
		"target",
		"get-ssh-address",
	}

	stdout, err := f.runFFXCmd(ctx, args...)
	if err != nil {
		return "", fmt.Errorf("ffx target get-ssh-address failed: %w", err)
	}

	return strings.TrimSpace(string(stdout)), nil
}

func (f *FFXTool) SupportsZedbootDiscovery(ctx context.Context) (bool, error) {
	// Check if ffx is configured to resolve devices in zedboot.
	args := []string{
		"config",
		"get",
		"discovery.zedboot.enabled",
	}

	stdout, err := f.runFFXCmd(ctx, args...)
	if err != nil {
		// `ffx config get` exits with 2 if variable is undefined.
		if exiterr, ok := err.(*exec.ExitError); ok {
			if exiterr.ExitCode() == 2 {
				return false, nil
			}
		}

		return false, fmt.Errorf("ffx config get failed: %w", err)
	}

	// FIXME(https://fxbug.dev/42060660): Unfortunately we need to parse the raw string to see if it's true.
	if string(stdout) == "true\n" {
		return true, nil
	}

	return false, nil
}

func (f *FFXTool) TargetAdd(ctx context.Context, target string) error {
	args := []string{"target", "add", "--nowait", target}
	_, err := f.runFFXCmd(ctx, args...)
	return err
}

func (f *FFXTool) Flasher() *Flasher {
	return newFlasher(f)
}

func (f *FFXTool) runFFXCmd(ctx context.Context, args ...string) ([]byte, error) {
	path, err := exec.LookPath(f.ffxToolPath)
	if err != nil {
		return []byte{}, err
	}

	// prepend a config flag for finding subtools that are compiled separately
	// in the same directory as ffx itself.
	args = append(
		[]string{
			"--log-level", "trace",
			"--isolate-dir", f.isolateDir.path,
			"--config", fmt.Sprintf("ffx.subtool-search-paths=%s", filepath.Dir(path)),
		},
		args...,
	)

	logger.Infof(ctx, "running: %s %q", path, args)
	cmd := exec.CommandContext(ctx, path, args...)
	var stdoutBuf bytes.Buffer
	cmd.Stdout = &stdoutBuf
	cmd.Stderr = os.Stderr

	cmdRet := cmd.Run()

	stdout := stdoutBuf.Bytes()
	if len(stdout) != 0 {
		logger.Infof(ctx, "%s", string(stdout))
	}

	if cmdRet == nil {
		logger.Infof(ctx, "finished running %s %q", path, args)
	} else {
		logger.Infof(ctx, "running %s %q failed with: %v", path, args, cmdRet)
	}
	return stdout, cmdRet
}

func (f *FFXTool) RepositoryCreate(ctx context.Context, repoDir, keysDir string) error {
	args := []string{
		"--config", "ffx_repository=true",
		"repository",
		"create",
		"--keys", keysDir,
		repoDir,
	}

	_, err := f.runFFXCmd(ctx, args...)
	return err
}

func (f *FFXTool) RepositoryPublish(ctx context.Context, repoDir string, packageManifests []string, additionalArgs ...string) error {
	args := []string{
		"repository",
		"publish",
	}

	for _, manifest := range packageManifests {
		args = append(args, "--package", manifest)
	}

	args = append(args, additionalArgs...)
	args = append(args, repoDir)

	_, err := f.runFFXCmd(ctx, args...)
	return err
}
