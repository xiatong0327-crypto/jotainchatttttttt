# jotainchatttttttt

Local Wi‑Fi **LAN chat + photo/file transfer** for **macOS and Windows** (same protocol: Mac ↔ Mac, Mac ↔ Windows, Windows ↔ Windows).

No accounts · no cloud · **no automatic updates** · traffic stays on your LAN.

| | |
|---|---|
| **Current version** | **[v0.2.1-group-chat](#v021-group-chat--2026-07-17)** |
| Previous | v0.2.0-cross-lan · [history](#version-history) |
| Platform | **macOS 12+** (arm64 packages here) · **Windows** (build on a Windows PC) |
| Cross-OS guide | [docs/windows-cross-lan.md](docs/windows-cross-lan.md) |
| Repo | https://github.com/xiatong0327-crypto/jotainchatttttttt |

---

## Features (implemented)

### Core (v0.1.0 baseline)

| Area | What you get |
|------|----------------|
| **Discovery** | Finds peers on the same Wi‑Fi (UDP). No pairing code. Shows peer OS. |
| **1:1 chat** | Text messages over TCP; sounds for incoming events (toggle in Settings). |
| **Identity** | Stable device id + display name; first-run onboarding. |
| **History** | Local SQLite; survives reinstall; delete one message / clear thread / clear all. |
| **File transfer** | Offer → **Accept / Reject / Cancel** → streaming data + whole-file **SHA-256**. |
| **Receive confirm** | Default: receiver must **Accept** before any file bytes flow. |
| **Single instance** | Only one app window per machine. |
| **Diagnostics** | Settings → Diagnostics (`DISC-*`, `TCP-*`, `XFER-*`, …). |
| **Packaging** | Mac: `npm run package:mac` → `.app` + zip. Windows: `npm run package:win` on a Windows host. |

### Resumable transfer (R1–R4, after v0.1.0)

| Area | What you get |
|------|----------------|
| **Interrupt keep** | Network glitch / kill keeps `*.partial` (not deleted). |
| **Resume** | Manual **Resume** button; optional **auto-resume** when peer reconnects. |
| **Persistence** | Transfer state in SQLite; survives app restart. |
| **Integrity** | Offer may carry SHA-256; trailer verified on complete. |
| **Cancel cleanup** | Cancel / delete message clears partial + token. |

### Send UX (v0.1.5+)

| How | Receiver Accept? | In chat |
|-----|------------------|---------|
| **File** button | **Always required** | Open / Show in folder when path known |
| **Drag & drop** into chat | **Always required** | Same |
| **Paste screenshot** ≤2 MB | **Not required** (auto-receive) | **Preview** + Open / Show in folder |

### Product rules

1. No auto-update — replace the app manually  
2. **Every file transfer needs Accept**, except **clipboard screenshot paste ≤2 MB**  
3. Discovery is enough to chat (no pairing)  
4. No group chat  
5. History stays until you delete it  
6. Product name: **jotainchatttttttt**  
7. Platform: **macOS + Windows** (same LAN wire protocol)  
8. **Group chat is text-only** (no files / screenshots in groups)

### Group chat (v0.2.1+)

| Action | How |
|--------|-----|
| **Create** | Sidebar → New group → name → get **join code** |
| **Join / verify** | Sidebar → Join code; a **member must be online** to accept the code |
| **Leave** | Open group → **Leave group** |
| **Files** | **Blocked** in groups (use 1:1) |

Details: [docs/group-chat.md](docs/group-chat.md).

---

## v0.2.1-group-chat — 2026-07-17

1. **Create group** with 6-character join code.  
2. **Join** by code (verified by any online member who has the group).  
3. **Leave group** stops delivery; history kept until Clear chat.  
4. **No documents/files/screenshots** in group (API + UI).  
5. Text mesh over existing 1:1 TCP sessions.

---

## v0.2.0-cross-lan — 2026-07-17

### Mac ↔ Windows

1. **Wire protocol unchanged** — discovery / chat / files already OS-agnostic; peers advertise `os` in announce.
2. **Code paths made portable** — Downloads dir (`HOME` / `USERPROFILE`), open & reveal (Finder / Explorer), sounds (Web Audio everywhere; `afplay` only on macOS), display-name hints.
3. **UI** — “Mac & Windows”, peer list shows OS, LAN help for both firewalls.
4. **Windows build** — run on a Windows PC: `npm run package:win` (see [docs/windows-cross-lan.md](docs/windows-cross-lan.md)). This Mac still ships arm64 `.app` via `package:mac`.

### Honest limits

- A **Windows `.exe`/NSIS installer is not produced on this Mac** — build Windows artifacts on Windows (or Windows CI).
- Same Wi‑Fi + firewall rules required; guest/AP isolation still blocks discovery.

---

## v0.1.5-open-finder — 2026-07-15

### High-value daily polish

1. **Open** — open local file with default macOS app (Preview, etc.).
2. **Show in Finder** — reveal/select the file in Finder.
3. **Empty states & copy** — clearer peer discovery steps; connected vs waiting; screenshot vs Accept policy.
4. **Settings → About / how to update** — version, ports, data paths, step-by-step replace-app update (history stays).
5. **Dual-Mac smoke checklist** — [docs/QA-dual-mac-smoke.md](docs/QA-dual-mac-smoke.md).

### Commands

- `open_local_path(path)`
- `reveal_in_finder(path)` (`open -R`)

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
| **v0.2.1** | `group-chat` | Text groups: join code, leave, no files |
| **v0.2.0** | `cross-lan` | Mac↔Windows ready codebase + Windows build guide |
| **v0.1.5** | `open-finder` | Open / Show in Finder; About update guide; QA smoke checklist |
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
packages/jotainchatttttttt-macos-arm64-v0.2.1-group-chat-YYYYMMDD.zip
```

Send the **ZIP**. Other Mac: unzip → double-click **Open-Me-First.command** (or right-click `.app` → Open).  
If blocked: Privacy & Security → Open Anyway, or `xattr -cr path/to/app`.  
**Mac arm64 packages** from this repo script. **Windows:** build on Windows — [docs/windows-cross-lan.md](docs/windows-cross-lan.md).

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
| [docs/QA-dual-mac-smoke.md](docs/QA-dual-mac-smoke.md) | Dual-Mac smoke checklist |
| [docs/windows-cross-lan.md](docs/windows-cross-lan.md) | Mac ↔ Windows build & firewall |
| [docs/group-chat.md](docs/group-chat.md) | Group chat join / leave / no files |

## Architecture hazards

Dual dial, sleep/roam, path traversal, SQLite writer, Local Network permission, AP isolation — see design §9–§10.
