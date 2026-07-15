# jotainchatttttttt

Local Wi‑Fi LAN chat + photo/file transfer. **macOS only** (Mac ↔ Mac). No accounts, no cloud, **no automatic updates**.

## Status

| Area | Status |
|------|--------|
| **Current version** | **v0.1.3-logo-auto2mb** (JOTAIN logo · auto-accept ≤2MB multi-format · drag-drop strengthened) |
| Previous | v0.1.2-screenshot-auto · v0.1.1-paste-dnd · Baseline v0.1.0 |
| Design | [docs/2026-07-14-design.md](docs/2026-07-14-design.md) |
| **v1 存档 / 下次续做** | **[docs/ARCHIVE-v1-handoff.md](docs/ARCHIVE-v1-handoff.md)** |
| PR1–PR7 | **Complete** — v1 可分发 |
| Diagnostics | [docs/diagnostics.md](docs/diagnostics.md) |
| Setup | [docs/setup-macos.md](docs/setup-macos.md) |
| QA | [docs/QA-checklist.md](docs/QA-checklist.md) |

### Version naming

- Baseline: **v0.1.0**
- Each feature batch: `v0.1.N-<keyword>` (e.g. `v0.1.1-paste-dnd`)
- You supply the keyword; agent bumps the patch and stamps the keyword.

### Send files (v0.1.3+)

1. **File** button — system picker → receiver must **Accept** (including image files)  
2. **Drag & drop** onto the chat panel / window → receiver must **Accept**  
3. **Paste screenshot** — capture then **⌘V** → auto-receive if ≤ **2 MB** (any common image format; mime optional)

## Product rules (v1)

1. No auto-update — manual install only  
2. File receive requires **Accept**  
3. Discovery is enough to chat (no pairing)  
4. No group chat  
5. History stays until you delete it; uninstall does not wipe data  
6. Name: **jotainchatttttttt**  
7. Platform: **macOS only**

## Develop

```bash
cd ~/Documents/jotainchatttttttt
npm install
npm run tauri:dev
```

## Release / distribute (use this)

```bash
npm run package:mac
```

```text
dist/jotainchatttttttt.app
dist/jotainchatttttttt-macos-arm64-v0.1.0-YYYYMMDD.zip
```

Send the **ZIP**. Other Mac: unzip → **right-click** app → **Open**.  
If blocked: Privacy & Security → Open Anyway, or `xattr -cr path/to/app`.  
**arm64 only** (Apple Silicon). Do not send bare `target/release/` binary. See [docs/setup-macos.md](docs/setup-macos.md).

## LAN ports

| Role | Port |
|------|------|
| Discovery | UDP **48765** |
| Control (chat + file signaling) | TCP **48766** |
| File data | TCP **48767** |

## Data locations

| What | Where |
|------|--------|
| Config, device id, messages DB | `~/Library/Application Support/com.jotain.jotainchatttttttt/` |
| Received files | `~/Downloads/jotainchatttttttt/` |

## Troubleshooting (empty peer list)

1. Same Wi‑Fi; not guest / client-isolated.  
2. System Settings → Privacy & Security → **Local Network** → enable this app.  
3. Allow firewall prompts for incoming connections.  
4. Settings → **Diagnostics** — search codes like `DISC-BIND-FAIL`, `TCP-LISTEN-FAIL`.  
5. Full guide: [docs/setup-macos.md](docs/setup-macos.md).

## Architecture hazards

Dual dial, sleep/roam, path traversal, SQLite writer, Local Network permission, AP isolation — see design §9–§10.
