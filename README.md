# jotainchatttttttt

Local Wi‑Fi **LAN chat + photo/file transfer** for **macOS only** (Mac ↔ Mac).

No accounts · no cloud · **no automatic updates** · traffic stays on your LAN.

| | |
|---|---|
| **Current version** | **[v0.1.4-screenshot-preview](#v014-screenshot-preview--2026-07-15)** |
| Previous | v0.1.3-logo-auto2mb · [history](#version-history) |
| Platform | macOS 12+ · **Apple Silicon (arm64)** |
| Repo | https://github.com/xiatong0327-crypto/jotainchatttttttt |

---

## Features (implemented)

### Core (v0.1.0 baseline)

| Area | What you get |
|------|----------------|
| **Discovery** | Finds other Macs on the same Wi‑Fi (UDP). No pairing code. |
| **1:1 chat** | Text messages over TCP; sounds for incoming events (toggle in Settings). |
| **Identity** | Stable device id + display name; first-run onboarding. |
| **History** | Local SQLite; survives reinstall; delete one message / clear thread / clear all. |
| **File transfer** | Offer → **Accept / Reject / Cancel** → streaming data + whole-file **SHA-256**. |
| **Receive confirm** | Default: receiver must **Accept** before any file bytes flow. |
| **Single instance** | Only one app window per Mac. |
| **Diagnostics** | Settings → Diagnostics (`DISC-*`, `TCP-*`, `XFER-*`, …). |
| **Packaging** | `npm run package:mac` → signed-ish ad-hoc `.app` + zip for AirDrop. |

### Resumable transfer (R1–R4, after v0.1.0)

| Area | What you get |
|------|----------------|
| **Interrupt keep** | Network glitch / kill keeps `*.partial` (not deleted). |
| **Resume** | Manual **Resume** button; optional **auto-resume** when peer reconnects. |
| **Persistence** | Transfer state in SQLite; survives app restart. |
| **Integrity** | Offer may carry SHA-256; trailer verified on complete. |
| **Cancel cleanup** | Cancel / delete message clears partial + token. |

### Send UX (v0.1.4+)

| How | Receiver Accept? | Chat preview |
|-----|------------------|--------------|
| **File** button | **Always required** (any size, including images) | Path only (images ≤2 MB also preview when local) |
| **Drag & drop** into chat | **Always required** | Same |
| **⌘V paste screenshot** ≤2 MB | **Not required** (auto-receive) | **Inline image preview** after send / receive |

### Product rules

1. No auto-update — replace the `.app` manually  
2. **Every file transfer needs Accept**, except **clipboard screenshot paste ≤2 MB**  
3. Discovery is enough to chat (no pairing)  
4. No group chat  
5. History stays until you delete it  
6. Product name: **jotainchatttttttt**  
7. Platform: **macOS only**

---

## v0.1.4-screenshot-preview — 2026-07-15

### Highlights

1. **Policy clarified**
   - **Only** ⌘V **screenshot paste** (≤2 MB, image payload) may set `autoAccept`.
   - File picker, drag-drop, and any staged non-paste bytes **always** require receiver **Accept** — even if the file is an image under 2 MB.
2. **Inline screenshot preview** in the chat bubble
   - Sender: preview appears immediately from local staging path.
   - Receiver: preview after auto-save (or when a completed image has a local path ≤2 MB).
3. **API**
   - `send_file_bytes(..., asScreenshotPaste: bool)` — paste path only passes `true`.
   - `read_local_image_preview(path)` — data-URL for chat UI (≤2 MB, magic-checked).

### Upgrade

Both sides should use **v0.1.4+** for previews and correct accept policy.

---

## v0.1.3-logo-auto2mb — 2026-07-15

### Highlights

1. **App icon** — JOTAIN Materials brand mark (from [jotainmaterials.com](https://www.jotainmaterials.com) favicon: white tile, navy + lime squares). Packaging fixes `CFBundleIconFile` (no `.icns` suffix) so Dock/Finder pick up the new icon.
2. **Screenshot auto-receive tightened**
   - Max size **2 MB** (was 25 MB).
   - Does **not** require `image/*` MIME: magic bytes (PNG/JPEG/GIF/WEBP/BMP/TIFF/HEIC/AVIF/…), extension, and loose OS types (`octet-stream`, `public.png`, empty type, …).
3. **Drag-and-drop hardened**
   - Tauri native path drop + HTML5 fallback on the chat panel.
   - Full-panel “Drop files here to send” overlay.
4. **Still requires Accept**: File button and **any dragged file** (including image files). Only **clipboard paste** of small images auto-accepts.

### Not in this release

- Folder send, pairing codes, group chat, Windows, E2E TLS, Developer ID notarization.

### Upgrade note

Both Macs should run **v0.1.3+** for auto-receive of paste screenshots. Older peers ignore `autoAccept` and still show Accept.

---

## Version history

| Version | Keyword | Summary |
|---------|---------|---------|
| **v0.1.4** | `screenshot-preview` | Chat image preview; only paste screenshots auto-accept |
| **v0.1.3** | `logo-auto2mb` | JOTAIN icon; paste auto-accept ≤2MB multi-format; drag-drop UX |
| **v0.1.2** | `screenshot-auto` | Paste screenshots skip Accept (`autoAccept` wire flag) |
| **v0.1.1** | `paste-dnd` | ⌘V paste image + drag-drop paths to send |
| **v0.1.0** | *(baseline)* | Chat, discovery, file offer/accept, resume R1–R4, package |

### Version naming

- Baseline: **v0.1.0**
- Later batches: `v0.1.N-<keyword>` (you pick the keyword; patch number increments)

---

## Develop

```bash
cd ~/Documents/jotainchatttttttt
npm install
npm run tauri:dev
```

```bash
cd src-tauri && cargo test
```

## Release / distribute

```bash
npm run package:mac
```

Output:

```text
packages/jotainchatttttttt.app
packages/jotainchatttttttt-macos-arm64-v0.1.4-screenshot-preview-YYYYMMDD.zip
```

Send the **ZIP**. Other Mac: unzip → double-click **Open-Me-First.command** (or right-click `.app` → Open).  
If blocked: Privacy & Security → Open Anyway, or `xattr -cr path/to/app`.  
**arm64 only**. See [docs/setup-macos.md](docs/setup-macos.md).

---

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
| Paste staging (sender) | `…/Application Support/…/outbound-staging/` |

## Troubleshooting (empty peer list)

1. Same Wi‑Fi; not guest / client-isolated.  
2. System Settings → Privacy & Security → **Local Network** → enable this app.  
3. Allow firewall prompts for incoming connections.  
4. Settings → **Diagnostics** — codes like `DISC-BIND-FAIL`, `TCP-LISTEN-FAIL`.  
5. Full guide: [docs/setup-macos.md](docs/setup-macos.md).

## Docs

| Doc | Purpose |
|-----|---------|
| [docs/2026-07-14-design.md](docs/2026-07-14-design.md) | System design |
| [docs/2026-07-14-resume-transfer-design.md](docs/2026-07-14-resume-transfer-design.md) | Resumable transfer design |
| [docs/2026-07-14-resume-qa-checklist.md](docs/2026-07-14-resume-qa-checklist.md) | Resume QA |
| [docs/ARCHIVE-v1-handoff.md](docs/ARCHIVE-v1-handoff.md) | v1 handoff |
| [docs/diagnostics.md](docs/diagnostics.md) | Diagnostic codes |
| [docs/setup-macos.md](docs/setup-macos.md) | Install / Local Network |
| [docs/QA-checklist.md](docs/QA-checklist.md) | General QA |

## Architecture hazards

Dual dial, sleep/roam, path traversal, SQLite writer, Local Network permission, AP isolation — see design §9–§10.
