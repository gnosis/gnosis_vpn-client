#!/bin/sh
systemctl disable gnosisvpn.service
systemctl stop gnosisvpn.service
systemctl daemon-reexec
systemctl daemon-reload
