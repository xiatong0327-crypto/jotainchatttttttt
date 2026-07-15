# Resume transfer QA checklist

Use two Macs on the same LAN, both on the build that includes PR-R1…R4.

## Happy path

- [ ] Send small file (&lt; 1 MiB) → Accept → complete; SHA shown on completed card
- [ ] Send multi‑hundred MB file → complete; path under Downloads/jotainchatttttttt
- [ ] Offer shows short SHA-256; receiver card stores offer hash before Accept

## Interrupt + resume (same process)

- [ ] Transfer to ~30% → disable Wi‑Fi on one side → **interrupted**, partial kept
- [ ] Re-enable Wi‑Fi → session green → **auto-resume** (if setting on) or tap **Resume**
- [ ] Completes; checksum OK; total upload ≈ size + small dirty-tail waste

## Process restart (R3)

- [ ] Transfer to ~30% → quit receiver app → restart → card **interrupted**, Resume works when sender still has the transfer
- [ ] Transfer to ~30% → quit sender → restart sender → receiver Resume / auto after both online
- [ ] Cancel after interrupt → partial + placeholder gone; cannot Resume

## Edge cases

- [ ] Accept then sender offline → ≤20s → first_data_timeout → Resume works
- [ ] Resume against busy sender → busy message → retry succeeds
- [ ] Source file deleted on sender → source_missing / re-send prompt
- [ ] Delete interrupted chat message → no ghost auto-resume
- [ ] Two same-named files Accept → different dest paths
- [ ] Settings: turn off auto-resume → reconnect does not auto-start; manual Resume still works

## Mixed / regression

- [ ] Text / Reject still works
- [ ] Chat text + sound still works
- [ ] Diagnostics show XFER-INTERRUPT / XFER-RESUME / XFER-HYDRATE as expected
