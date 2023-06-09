# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
import argparse
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
import os
import shutil
import subprocess
from typing import List
from urllib.parse import parse_qs, urlparse


class PubSub:

    def __init__(self):
        self.subscribers = {}

    def subscribe(self, topic, config):
        if topic not in self.subscribers:
            self.subscribers[topic] = []
        self.subscribers[topic].append(config)

    def publish(self, topic, path):
        for config in self.subscribers.get(topic, []):
            repo = os.path.expanduser(config['repo'])
            cmds = config['commands']
            dest_path = config['listen_to'][topic]
            publish_table = config.get('publish', {})

            if dest_path == None:
                dest_path = path
            if dest_path != path:
                shutil.copy(path, os.path.join(repo, dest_path))

            execute_commands_in_repo(repo, cmds, dest_path)
            # Publish downstream messages
            for topic, message in publish_table.items():
                pub.publish(topic, os.path.join(
                    repo, message))


pub = PubSub()


class PubSubHandler(BaseHTTPRequestHandler):

    def do_GET(self):
        url_parts = urlparse(self.path)
        query_params = parse_qs(url_parts.query)
        topic = query_params.get('event')[0]
        message = query_params.get('message')[0]
        self.send_response(200)
        self.end_headers()

        pub.publish(topic, message)


def execute_commands_in_repo(repo: str, commands: List[str], dest_path: str):
    print(f'Run in repo: {repo}')
    for command in commands:
        cmd = command.format(dest_path=dest_path).split()
        print(f'Execute cmd: {cmd}')
        result = subprocess.run(cmd, cwd=repo)


def register_config(config_path: str):
    if not config_path:
        return
    with open(config_path, 'r') as f:
        rule_table = json.load(f)

    for _, config in rule_table.items():
        listen_to = config['listen_to']
        # Register config for each listened events
        for topic in listen_to:
            pub.subscribe(topic, config)


def run_server():
    parser = argparse.ArgumentParser()
    parser.add_argument(
        '--config',
        help='A path to the config file.',
    )
    args = parser.parse_args()

    # Parse configuration file
    register_config(args.config)

    # Start server
    server_address = ('localhost', 8000)
    httpd = HTTPServer(server_address, PubSubHandler)
    print('Starting server...')
    httpd.serve_forever()


if __name__ == '__main__':
    run_server()
