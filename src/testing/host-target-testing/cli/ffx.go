// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package cli

import (
	"context"
	"flag"
	"os"

	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/ffx"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/util"
)

type FfxConfig struct {
	ffxPath           string
	ffxIsolateDirPath string
	ffx               *ffx.FFXTool
}

func NewFfxConfig(fs *flag.FlagSet) *FfxConfig {
	c := &FfxConfig{}
	fs.StringVar(&c.ffxPath, "ffx-path", "host-tools/ffx", "ffx tool path")
	fs.StringVar(&c.ffxIsolateDirPath, "ffx-isolate-dir", "", "ffx isolate dir path")

	return c
}

func (c *FfxConfig) Validate() error {
	for _, s := range []string{
		c.ffxPath,
		c.ffxIsolateDirPath,
	} {
		if err := util.ValidatePath(s); err != nil {
			return err
		}
	}
	return nil
}

func (c *FfxConfig) NewFfxIsolateDir(ctx context.Context) (ffx.IsolateDir, func(), error) {
	var ffxIsolateDirPath string
	var cleanup func()
	var err error
	if c.ffxIsolateDirPath == "" {
		ffxIsolateDirPath, err = os.MkdirTemp("", "ffx-isolate-dir")
		if err != nil {
			return ffx.IsolateDir{}, func() {}, err
		}
		cleanup = func() {
			os.RemoveAll(ffxIsolateDirPath)
		}
	} else {
		cleanup = func() {}
	}

	return ffx.NewIsolateDir(ffxIsolateDirPath), cleanup, nil
}

func (c *FfxConfig) NewFfxTool(ctx context.Context) (*ffx.FFXTool, func(), error) {
	var ffxIsolateDirPath string
	var cleanupDir func()
	var err error
	if c.ffxIsolateDirPath == "" {
		ffxIsolateDirPath, err = os.MkdirTemp("", "ffx-isolate-dir")
		if err != nil {
			return nil, func() {}, err
		}
		cleanupDir = func() {
			os.RemoveAll(ffxIsolateDirPath)
		}
	} else {
		ffxIsolateDirPath = c.ffxIsolateDirPath
		cleanupDir = func() {}
	}

	ffxIsolateDir := ffx.NewIsolateDir(ffxIsolateDirPath)
	ffx, err := ffx.NewFFXTool(c.ffxPath, ffxIsolateDir)
	if err != nil {
		cleanupDir()
		return nil, func() {}, err
	}

	return ffx, func() {
		ffx.StopDaemon(ctx)
		cleanupDir()
	}, nil
}
