# HOPR sessions

A connection uses three HOPR sessions, all through the same exit. The connect order is in
[connection_sequence.mmd](connection_sequence.mmd).

| Session          | Kind | Target                  | Paid           | Lifetime            |
| ---------------- | ---- | ----------------------- | -------------- | ------------------- |
| Probe (bridge)   | TCP  | gvpn server at the exit | no             | per probed exit     |
| Relayer side-car | TCP  | `api.curvy.box:443`     | no             | per connect attempt |
| WireGuard        | UDP  | WG server at the exit   | yes (`UsePIX`) | per connect attempt |

## Probe session

- Opened by the probe (`probe.rs`) when an exit is probed or targeted. Checks version, ping and
  load, then counts as ready.
- A connect starts only on a ready probe. The WG public key is registered over it, and a
  disconnect unregisters over it.
- It stays open while a connection, a disconnect or a waiting target uses it.
- A quick probe (list sweeps) opens a short, unbalanced session; a probe starting meanwhile
  adopts it instead of opening a second one.

## Relayer side-car

- Carries every Curvy relayer call: fee quote, allocate, proof submit.
- Opens first, alongside killswitch lockdown, WG keys and registration, so the deposit never waits
  for the tunnel it pays for.
- Bound to a local port fixed per worker start. edgli resolves `api.curvy.box` to that port; TLS
  still checks `api.curvy.box`. One connection is reused for all calls.
- Free at the exit: the allow-list admits the target and the admission rule does not enforce PIX.
  Its SURBs come from their own `relayer` tier.
- PIX deposits are held while the killswitch is armed and no side-car is open
  (`pix_deposits_held`).

## WireGuard session

- Opened after registration and the side-car, spliced into the NepTUN pump.
- The exit asks for the deposit on this session. The payment then runs over the side-car.
- Until the exit serves the session, the killswitch lets only the gateway through, and the
  worker pings it (`AwaitingPayment`). The first answer confirms the payment; then the tunnel
  opens to user traffic.
- Later chunks follow the same path. A late one shows as an in-session stall.

## Failure and teardown

| Event              | WireGuard       | Side-car         | Held deposits        | Probe                  |
| ------------------ | --------------- | ---------------- | -------------------- | ---------------------- |
| Attempt fails      | reopened        | closed, reopened | held through backoff | kept                   |
| Retries exhausted  | worker restarts | worker restarts  | lost with the worker | worker restarts        |
| Disconnect         | closed          | closed           | dropped, refunded    | unregisters, then free |
| Reconnect in place | reopened        | closed, reopened | dropped, refunded    | kept                   |

A deposit already inside the pool's `allocate` is past the hold: if the attempt fails, it can
still pay for the closed session once the next side-car is up.
