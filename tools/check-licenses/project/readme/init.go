// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package readme

import (
	"context"
	"fmt"

	"go.fuchsia.dev/fuchsia/tools/check-licenses/util"
)

var git util.GitInterface

func Initialize() error {
	var err error
	git, err = util.NewGit()
	if err != nil {
		return fmt.Errorf("Failed to create git hook: %w", err)
	}
	return nil
}

func InitializeForTest() {
	git = gitForTest{}
}

type gitForTest struct {
}

func (g gitForTest) GetURL(ctx context.Context, path string) (string, error) {
	return "www.example.com", nil
}

func (g gitForTest) GetCommitHash(ctx context.Context, path string) (string, error) {
	return "hash", nil
}
