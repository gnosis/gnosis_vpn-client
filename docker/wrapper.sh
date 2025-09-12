#!/usr/bin/env bash

set -o errexit

declare dest1="$DESTINATION_ADDRESS_1"
if [ -z "$dest1" ]; then
    echo "DESTINATION_ADDRESS_1 is not set"
    exit 1
fi

declare dest2="$DESTINATION_ADDRESS_2"
if [ -z "$dest2" ]; then
    echo "DESTINATION_ADDRESS_2 is not set"
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

awk -v cont="$port" '{gsub(/endpoint = "http:\/\/host.docker.internal:<api port>"/, "endpoint = \"http://host.docker.internal:" cont "\""); print}' config.toml >temp.toml && mv temp.toml config.toml
awk -v cont="$token" '{gsub(/api_token = "<api token>"/, "api_token = \"" cont "\""); print}' config.toml >temp.toml && mv temp.toml config.toml
awk -v cont="$dest1" '{gsub(/destinations."<address1>"/, "destinations." cont ); print}' config.toml >temp.toml && mv temp.toml config.toml
awk -v cont="$dest2" '{gsub(/destinations."<address2>"/, "destinations." cont ); print}' config.toml >temp.toml && mv temp.toml config.toml

./gnosis_vpn -c config.toml
