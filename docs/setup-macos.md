# macOS setup — jotainchatttttttt

## Requirements

- macOS 12+ (Apple Silicon or Intel)
- Two Macs on the **same Wi‑Fi / LAN** for chat and files
- **Local Network** permission when prompted

## Install (release build)

### Recommended: package script (fixes signature)

```bash
cd ~/Documents/jotainchatttttttt
npm install
npm run package:mac
```

Output:

```text
dist/jotainchatttttttt.app
dist/jotainchatttttttt-macos-arm64-v0.1.0-YYYYMMDD.zip   # (or x86_64)
```

**Send the ZIP** (not the bare file under `target/release/`).

### On the other Mac

1. Unzip → get `jotainchatttttttt.app`  
2. **Right-click** → **Open** → **Open** (required for ad-hoc / unsigned builds)  
3. If blocked: System Settings → Privacy & Security → **Open Anyway**  
4. Still blocked (Terminal):

   ```bash
   xattr -cr ~/Downloads/jotainchatttttttt.app
   open ~/Downloads/jotainchatttttttt.app
   ```

### Architecture

- Built on Apple Silicon → **arm64 only** (M1/M2/M3/M4)  
- Built on Intel Mac → **x86_64 only**  
- Wrong CPU → macOS says app is damaged / can’t open. Rebuild on matching machine (or later: universal binary).

### Why “can’t open” on the other computer (common)

| Cause | Fix |
|-------|-----|
| Incomplete codesign from raw `tauri build` | Use `npm run package:mac` |
| Gatekeeper quarantine (downloaded zip) | Right-click Open, or `xattr -cr` |
| Opened bare binary, not `.app` | Only open `jotainchatttttttt.app` |
| Intel vs Apple Silicon mismatch | Build on same arch as recipient |
| Double-clicked zip contents incorrectly | Unzip first, then open `.app` |

There is **no automatic update**. Replace the app manually when you ship a new build.

## First launch

1. Set a **display name** (peers see this).
2. Allow **Local Network** if macOS asks.
3. Open Settings and confirm Discovery shows **Running**.

## Same Wi‑Fi checklist

If the device list stays empty:

1. Both apps open, same Wi‑Fi (not guest / client-isolated network).
2. System Settings → Privacy & Security → **Local Network** → enable jotainchatttttttt.
3. Firewall: allow incoming for the app if prompted.
4. Ports free / not blocked:

   | Role | Port |
   |------|------|
   | Discovery | UDP **48765** |
   | Chat / signaling | TCP **48766** |
   | File bytes | TCP **48767** |

5. Settings → **Diagnostics** — look for `DISC-BIND-FAIL`, `TCP-LISTEN-FAIL`, etc.

## Data (survives uninstall)

| What | Path |
|------|------|
| Config + device id + SQLite | `~/Library/Application Support/com.jotain.jotainchatttttttt/` |
| Received files | `~/Downloads/jotainchatttttttt/` |

Dragging the app to Trash does **not** delete Application Support. Clear history in Settings, or delete the folder manually.

## Develop

```bash
npm run tauri:dev
```

## No cloud / no accounts

All traffic is LAN-only through your router. No login, no telemetry, no auto-update phone-home.
