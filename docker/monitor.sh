#!/usr/bin/env bash

set -o errexit
set -x

declare address="$ADDRESS"
declare private_key="$PRIVATE_KEY"
declare server_public_key="$SERVER_PUBLIC_KEY"
if [ -z "$address" ]; then
  echo "ADDRESS is not set"
  exit 1
fi
if [ -z "$private_key" ]; then
  echo "PRIVATE_KEY is not set"
  exit 1
fi
if [ -z "$server_public_key" ]; then
  echo "SERVER_PUBLIC_KEY is not set"
  exit 1
fi

awk -v cont="$address" '{gsub(/Address = <address>/, "Address = " cont); print}' wgclient.conf > temp.conf && mv temp.conf wgclient.conf
awk -v cont="$private_key" '{gsub(/PrivateKey = <private key>/, "PrivateKey = " cont); print}' wgclient.conf > temp.conf && mv temp.conf wgclient.conf
awk -v cont="$server_public_key" '{gsub(/PublicKey = <server public key>/, "PublicKey = " cont); print}' wgclient.conf > temp.conf && mv temp.conf wgclient.conf

wg-quick up ./wgclient.conf
while true; do
    wg show wgclient
    sleep 10
done
