# FAQ

## Unable to Access Websites After Activating Gnosis VPN

If you’ve activated Gnosis VPN but cannot open any website, follow these steps to diagnose and resolve the issue:

### Test Website Access

- Double-check your Firefox proxy settings as outlined in [step 6](./ONBOARDING.md#6-use-gnosisvpn-connection-to-browse-the-internet).
- Navigate to [example.com](https://example.com/).

Can you access that site?

- If **no** proceed to the next step.

### Verify Gnosis VPN Connection

- Navigate to the terminal where you launched the Gnosis VPN client.
- On successful connection, Gnosis VPN will display the following message:

```
    /---============================---\
    |   VPN CONNECTION ESTABLISHED   |
    \---============================---/
```

Did you see that message?

- If **no**, check the log output for any errors.
- Ensure you provided a valid entry node and API token in the configuration file.
- Ensure you have an open payment channel to the relay nodes. See [step 2](./ONBOARDING.md#2-enable-gnosisvpn-to-establish-connections-to-the-exit-nodes-from-your-hoprd-node).

- If the service does **not** appear to be doing anything, proceed to the next step.

### Use Gnosis VPN Control Application to check status

- In a separate terminal, run the Gnosis VPN control application to check the status:

`<some_path>/gnosis_vpn-ctl status`

Did you see the list of available destinations?

- If **no**, rerun the installer.
- If **yes**, try connecting to a different location:

`<some_path>/gnosis_vpn-ctl connect <destination peer id>`
