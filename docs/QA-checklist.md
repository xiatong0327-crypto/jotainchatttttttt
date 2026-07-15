# QA checklist (Mac v1)

Use two physical Macs on the same Wi‑Fi unless noted.

## Install & identity

- [ ] App launches (dev or `.app`)
- [ ] First-run display name required
- [ ] Settings shows version, bundle id, **auto-update disabled**
- [ ] Device id stable after quit/relaunch
- [ ] Reinstall without deleting Application Support keeps history + id

## Discovery

- [ ] Both Macs appear in each other’s Devices list within ~5s
- [ ] Deny Local Network → empty list + help text (not silent hang)
- [ ] Status → Running; ports 48765 / 48766 / 48767 documented
- [ ] Peer goes offline after quit (~6–30s)
- [ ] Diagnostics: `DISC-BIND`, `DISC-PEER-SEEN` on success

## Chat

- [ ] Status becomes **connected** (green) after link
- [ ] Text both directions
- [ ] History after quit/relaunch
- [ ] Offline send fails cleanly (failed status / error banner)
- [ ] Sleep/wake: reconnect or clean fail (no permanent false “connected”)

## History

- [ ] Delete single message (local only)
- [ ] Clear chat (confirm)
- [ ] Clear all history (double confirm); identity kept
- [ ] Uninstall app only → data folder remains

## Files (confirm by default)

- [ ] File button only useful when connected
- [ ] Offer appears on receiver with **Accept / Reject**
- [ ] No file written until Accept
- [ ] Reject → sender sees rejected
- [ ] Accept → progress → completed path under Downloads
- [ ] Cancel mid-transfer
- [ ] Large multi-100MB file completes; content intact
- [ ] Diagnostics: `XFER-OFFER-*` → `XFER-ACCEPT` → `XFER-COMPLETE`

## Packaging / policy

- [ ] No updater / Sparkle / phone-home version API
- [ ] Manual `.app` / `.dmg` only
- [ ] Gatekeeper path documented for unsigned builds
- [ ] Single-instance: second launch focuses first window

## Diagnostics

- [ ] Settings → Diagnostics shows codes
- [ ] Filter Warn+ / Error works
- [ ] Clear log works
