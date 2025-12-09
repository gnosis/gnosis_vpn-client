# Routing solutions

## Linux potential solution 1: fwmark + ip rule

- setsocketopts marks all packets with an fwmark
- ip rule specifies routing table based on fwmark

Problem:
- requires socket level access
- incoming traffic routing

## Linux potential solution 2: iptables + ip rule

- iptables marks outgoing packets from user with fwmark
- iptables stores outgoing packets with that fwmark in lookup table
- iptables preroutes incoming packets accordingly
- ip rule specifies routing table based on uid and fwmark (not sure if needed)

Problem:
- relies on iptables firewall
- works only for child process
- does not work system wide
- able to ping 10.128.0.1 from root/user
- with additional ip rule able to ping 10.128.0.1 from worker as well

## macOS potential solution: pf + route

- add pf rule to bypass all uid related traffic
- since stateful firewall incoming packets should be rerouted the same

Problem:
- does not seem to work at all
- all system traffic seems to be routet regardless of pf settings
- pf firewall not necessarily enabled on macOS by default
