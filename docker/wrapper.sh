#!/usr/bin/env bash

set -o errexit

declare ip="$ADDRESS"
if [ -z "$ip" ]; then
  echo "ADDRESS is not set"
  exit 1
fi

declare priv_key="$PRIVATE_KEY"
if [ -z "$priv_key" ]; then
  echo "PRIVATE_KEY is not set"
  exit 1
fi

declare server_pub_key="$SERVER_PUBLIC_KEY"
if [ -z "$server_pub_key" ]; then
  echo "SERVER_PUBLIC_KEY is not set"
  exit 1
fi

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

awk -v cont="$port" '{gsub(/endpoint = "http:\/\/host.docker.internal:<api port>"/, "endpoint = \"http://host.docker.internal:" cont "\""); print}' config.toml > temp.toml && mv temp.toml config.toml
awk -v cont="$token" '{gsub(/api_token = "<api token>"/, "api_token = \"" cont "\""); print}' config.toml > temp.toml && mv temp.toml config.toml
awk -v cont="$dest" '{gsub(/destination = "<destination peer id>"/, "destination = \"" cont "\""); print}' config.toml > temp.toml && mv temp.toml config.toml
awk -v cont="$ip" '{gsub(/address = "<wg address>"/, "address = \"" cont "\""); print}' config.toml > temp.toml && mv temp.toml config.toml
awk -v cont="$priv_key" '{gsub(/private_key = "<client private key>"/, "private_key = \"" cont "\""); print}' config.toml > temp.toml && mv temp.toml config.toml
awk -v cont="$server_pub_key" '{gsub(/server_public_key = "<server public key>"/, "server_public_key = \"" cont "\""); print}' config.toml > temp.toml && mv temp.toml config.toml

GNOSISVPN_CONFIG_PATH=./config.toml ./gnosis_vpn
