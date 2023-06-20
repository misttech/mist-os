// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package file

import (
	"encoding/json"
	"fmt"
	"regexp"

	classifierLib "github.com/google/licenseclassifier/v2"
)

const (
	defaultClassifierThreshold = 0.8
)

var (
	AllFiles        map[string]*File
	AllLicenseFiles map[string]*File
	urlRegex        *regexp.Regexp

	spdxFileIndex     int
	spdxFileDataIndex int

	classifier *classifierLib.Classifier
)

func init() {
	AllFiles = make(map[string]*File, 0)
	AllLicenseFiles = make(map[string]*File, 0)
	Config = NewConfig()

}

func Initialize(c *FileConfig) error {
	if c.ClassifierThreshold == 0 {
		c.ClassifierThreshold = defaultClassifierThreshold
	}
	classifier = classifierLib.NewClassifier(c.ClassifierThreshold)
	for _, path := range c.ClassifierLicensePaths {
		err := classifier.LoadLicenses(path)
		if err != nil {
			return fmt.Errorf("Failed to load license texts from path %s: %w", path, err)
		}
	}

	var err error
	urlRegex, err = regexp.Compile(`.*googlesource\.com\/([^\+]+\/)\+.*`)
	if err != nil {
		return err
	}

	// Save the config file to the out directory (if defined).
	if b, err := json.MarshalIndent(c, "", "  "); err != nil {
		return err
	} else {
		plusFile("_config.json", b)
	}

	Config = c
	return nil
}
