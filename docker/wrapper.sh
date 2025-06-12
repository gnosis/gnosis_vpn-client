#!/usr/bin/env bash

set -o errexit

declare dest="$DESTINATION_PEER_ID"
if [ -z "$dest" ]; then
    echo "DESTINATION_PEER_ID is not set"
    exit 1
fi

declare port="$API_PORT"
if [ -z "$port" ]; then
    echo "API_PORT is not set"
    exit 1
fi

declare token="$API_TOKEN"
if [ -z "$token" ]; then
    echo "API_TOKEN is not set"
    exit 1
fi

declare lport="$LISTEN_PORT"
if [ -z "$lport" ]; then
    echo "LISTEN_PORT is not set"
    exit 1
fi

awk -v cont="$port" '{gsub(/endpoint = "http:\/\/host.docker.internal:<api port>"/, "endpoint = \"http://host.docker.internal:" cont "\""); print}' config.toml >temp.toml && mv temp.toml config.toml
awk -v cont="$token" '{gsub(/api_token = "<api token>"/, "api_token = \"" cont "\""); print}' config.toml >temp.toml && mv temp.toml config.toml
awk -v cont="$dest" '{gsub(/destinations.<peer id>/, "destinations." cont ); print}' config.toml >temp.toml && mv temp.toml config.toml
awk -v cont="$lport" '{gsub(/listen_port = "<listen port>"/, "listen_port = " cont ); print}' config.toml >temp.toml && mv temp.toml config.toml

./gnosis_vpn -c config.toml
