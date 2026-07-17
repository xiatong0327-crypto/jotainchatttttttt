# Mac ↔ Windows LAN (v0.2.0-cross-lan)

## Short answer

**Yes.** The chat/file **wire protocol is OS-agnostic** (UDP discovery + TCP control + TCP file data).  
A Mac running jotainchatttttttt and a Windows PC on the **same Wi‑Fi** can discover each other, chat, and transfer files **if both run a build that speaks `PROTOCOL_VERSION = 1`**.

This Mac-only workspace can still **produce the macOS package**. The **Windows installer must be built on a Windows machine** (or CI with Windows runners).

---

## What already interops

| Layer | Shared? |
|-------|---------|
| Discovery UDP **48765** JSON announce (`v`, `deviceId`, `displayName`, `os`, …) | Yes |
| Control TCP **48766** (hello, text, fileOffer/Accept/…, resume) | Yes |
| File data TCP **48767** + SHA-256 | Yes |
| `messages.db` shape | Same idea per machine (not shared across OS installs) |
| Accept / screenshot auto-accept rules | Same product rules |

Announce already sends `os: "macos" | "windows" | …` so the peer list can show the other side’s OS.

---

## Platform differences (implemented in code)

| Concern | macOS | Windows |
|---------|-------|---------|
| Receive folder | `~/Downloads/jotainchatttttttt/` | `%USERPROFILE%\Downloads\jotainchatttttttt\` |
| Open file | `open` | `cmd /C start` |
| Reveal in folder | `open -R` (UI: “Show in folder”) | `explorer /select,` |
| Sound backup | `afplay` | Web Audio only |
| Suggested name | ComputerName / hostname | `%COMPUTERNAME%` / hostname |
| Firewall | Local Network permission | Private network + allow app |

---

## Build Windows binary (on a Windows PC)

Prereqs (same as Tauri 2 Windows):

- Rust stable  
- Node.js + npm  
- [WebView2](https://developer.microsoft.com/en-us/microsoft-edge/webview2/)  
- Visual Studio C++ build tools  

```bat
git clone https://github.com/xiatong0327-crypto/jotainchatttttttt.git
cd jotainchatttttttt
npm install
npm run package:win
```

Or:

```bat
npm run tauri:build
```

Look under `src-tauri\target\release\bundle\` for NSIS installer / exe.

**Note:** `package:mac` stays macOS-only (run on a Mac). Do not expect a Windows `.exe` from `npm run package:mac`.

---

## Mac ↔ Windows checklist

1. Same Wi‑Fi (not guest / AP isolation).  
2. Mac: Local Network allowed for the app.  
3. Windows: network profile **Private**; allow jotainchatttttttt through Windows Firewall (inbound UDP 48765, TCP 48766–48767).  
4. Both apps open; wait until peer is **online** then **connected (green)**.  
5. Text chat both ways.  
6. File from Mac → Windows: Accept on Windows.  
7. File from Windows → Mac: Accept on Mac.  
8. Optional: ⌘V / Win+Shift+S paste screenshot ≤2 MB auto-receive if both on v0.1.4+ policies.

---

## Firewall one-liners (Windows, admin PowerShell — optional)

```powershell
New-NetFirewallRule -DisplayName "jotainchat UDP 48765" -Direction Inbound -Protocol UDP -LocalPort 48765 -Action Allow
New-NetFirewallRule -DisplayName "jotainchat TCP 48766-48767" -Direction Inbound -Protocol TCP -LocalPort 48766,48767 -Action Allow
```

---

## Out of scope (still)

- Building Windows installers **from** this Mac without a Windows host/CI  
- Cross-compilation Tauri GUI (possible but fragile; prefer native Windows build)  
- Linux as first-class (code paths partially ready via `xdg-open`, not packaged)  
- Internet / NAT traversal (LAN only)

---

## Version

**v0.2.0-cross-lan** — codebase ready for Windows peers; ship Windows artifact when you build on Windows.
