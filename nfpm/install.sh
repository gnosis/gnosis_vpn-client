#!/bin/sh
systemctl daemon-reexec
systemctl daemon-reload
systemctl enable gnosisvpn.service
systemctl start gnosisvpn.service
