#!/usr/bin/env bash

set -o errexit

declare ip="$ADDRESS"
declare priv_key="$PRIVATE_KEY"
declare server_pub_key="$SERVER_PUBLIC_KEY"
if [ -z "$ip" ]; then
  echo "ADDRESS is not set"
  exit 1
fi
if [ -z "$priv_key" ]; then
  echo "PRIVATE_KEY is not set"
  exit 1
fi
if [ -z "$server_pub_key" ]; then
  echo "SERVER_PUBLIC_KEY is not set"
  exit 1
fi

awk -v cont="$ip" '{gsub(/Address = <address>/, "Address = " cont); print}' wgclient.conf > temp.conf && mv temp.conf wgclient.conf
awk -v cont="$priv_key" '{gsub(/PrivateKey = <private key>/, "PrivateKey = " cont); print}' wgclient.conf > temp.conf && mv temp.conf wgclient.conf
awk -v cont="$server_pub_key" '{gsub(/PublicKey = <server public key>/, "PublicKey = " cont); print}' wgclient.conf > temp.conf && mv temp.conf wgclient.conf

chmod 600 wgclient.conf
wg-quick up ./wgclient.conf

while true; do
    wg show wgclient
    sleep 10
done
