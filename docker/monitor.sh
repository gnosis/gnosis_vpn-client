#!/usr/bin/env sh

wg-quick up ./wgclient.conf
while true; do
    wg show wgclient
    sleep 10
done
