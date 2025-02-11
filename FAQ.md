# FAQ

## Unable to Access Websites After Activating GnosisVPN

If you’ve activated GnosisVPN but cannot open any websites from the [allow list](https://gnosisvpn.com/servers#allowlist), follow these steps in order to diagnose and resolve the issue:

### Test Website Access

- Double check your Firefox proxy settings as outlined in [step 12](./ONBOARDING.md#12-use-gnosisvpn-connection-to-browse-the-internet-macos).
- Navigate to [example.com](https://example.com/).

Can you access that site?

- If **yes**, everything works as long as you adhere to the [allow list](https://gnosisvpn.com/servers#allowlist).

### Verify WireGuard connection

- Ensure that WireGuard is running on your machine and that the tunnel has an active status.
  - macOS users: Locate the WireGuard app icon in the top-right corner of the screen, click on "Manage Tunnels".
    Verify that the tunnel status is "Active".
  - Linux users: Run the command: `sudo wg show`. The output displays an interface named `gnosisvpnpoc`.

Do you see an **active** WireGuard tunnel?

- If **no**, verify that you followed [step 11](./ONBOARDING.md#11-update-the-newly-created-wireguard-tunnel-and-launch-wireguard-macos) closely.

- Check that WireGuard can establish a handshake.
  - macOS users: Locate the WireGuard app icon in the top-right corner of the screen, click on "Manage Tunnels".
    Locate "Data Received:" field of the active tunnel.
  - Linux users: Run the command: `sudo wg show`. Either locate `latest handshake:` or `transfer:` entries.

Do you see more that `0 B` (zero bytes) received or have a `latest handshake` entry?

- If **no**, verify that you forward your ports correctly, see [step 5](./ONBOARDING.md#5-configure-your-hoprd-node-to-allow-gnosisvpn-connections-macos).

### Verify Hoprd connection

On successful startup GnosisVPN will display the following message:

```
    /---============================---\
    |   HOPRD CONNECTION ESTABLISHED   |
    \---============================---/
```

Did you see that message?

- If **no**, check the log output for any errors.
- Ensure you provided a valid entry node / APItoken in the configuration file.
- Ensure you have a working route to the exit node, see [step 8](./ONBOARDING.md#8-enable-gnosisvpn-to-establish-connections-to-the-exit-nodes-from-your-hoprd-node-macos).
