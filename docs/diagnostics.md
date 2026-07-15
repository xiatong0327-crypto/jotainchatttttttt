# Diagnostics — logic node codes

Every critical path logs with a **stable code**. Use these to locate bugs without re-reading the whole codebase.

## Where to look

| Surface | Format |
|---------|--------|
| Terminal (stderr) | `[JC][<CODE>][<LEVEL>] <message>` |
| App → Settings → **Diagnostics** | Newest first; filter All / Warn+ / Error |
| Event | `diagnostic` (warn/error only, live UI) |

Ring buffer keeps the last **200** entries in memory (not on disk).

## Code catalog

### Lifecycle
| Code | When |
|------|------|
| `APP-START` | Process starts |
| `APP-STATE-READY` | Config/DB loaded; discovery + TCP starting |

### Config / identity
| Code | When |
|------|------|
| `CFG-LOAD` | Config loaded successfully |
| `CFG-LOAD-CORRUPT` | JSON corrupt → quarantined + new id |
| `CFG-LOAD-REPAIR` | Inconsistent fields auto-repaired |
| `CFG-SAVE` / `CFG-SAVE-FAIL` | Persist config |
| `CFG-SET-NAME` / `CFG-SET-NAME-FAIL` | Display name change |

### Discovery (UDP)
| Code | When |
|------|------|
| `DISC-BIND` / `DISC-BIND-FAIL` | Bind discovery port 48765 |
| `DISC-ANNOUNCE-FAIL` | Broadcast send failed |
| `DISC-RECV-FAIL` | UDP recv hard error |
| `DISC-PEER-SEEN` | **New** peer first seen |
| `DISC-PEER-EXPIRE` | Peer(s) went offline (TTL) |
| `DISC-STOP` | Discovery loop stop/exit |

### Session (TCP)
| Code | When |
|------|------|
| `TCP-LISTEN` / `TCP-LISTEN-FAIL` | Bind control port 48766 |
| `TCP-ACCEPT` / `TCP-ACCEPT-FAIL` | Inbound connection |
| `TCP-DIAL-START` | Begin outbound connect |
| `TCP-DIAL-FAIL` | Connect timeout/error |
| `TCP-DIAL-SPAWN-FAIL` | Thread spawn failed (dialing stuck risk) |
| `TCP-HELLO-OK` / `TCP-HELLO-FAIL` | Handshake result |
| `TCP-ARB-DROP` | Dual-connect arbitration dropped a side |
| `TCP-ARB-REPLACE` | Replaced wrong-side session |
| `TCP-SESSION-UP` | Session registered |
| `TCP-SESSION-DOWN` | Session removed from map |
| `TCP-PING-FAIL` | Keepalive write failed (often after sleep) |
| `TCP-RECONCILE-DROP` | Dropped due to offline / IP change |
| `TCP-READ-FAIL` | Read I/O error on session |
| `TCP-FRAME-BAD` | Bad/oversized frame |

### Messaging
| Code | When |
|------|------|
| `MSG-SEND` / `MSG-SEND-FAIL` | Outbound text |
| `MSG-RECV` / `MSG-RECV-DUP` / `MSG-RECV-REJECT` | Inbound text |
| `MSG-PERSIST-FAIL` | SQLite insert failed |

### History
| Code | When |
|------|------|
| `HIST-DELETE` | Single message deleted |
| `HIST-CLEAR-PEER` | Thread cleared |
| `HIST-CLEAR-ALL` | All history cleared |
| `HIST-DELETE-FAIL` | Delete/clear error |

### Database
| Code | When |
|------|------|
| `DB-OPEN` / `DB-OPEN-FAIL` | Open messages.db |
| `DB-QUERY-FAIL` | list_messages etc. failed |

### File transfer
| Code | When |
|------|------|
| `XFER-LISTEN` / `XFER-LISTEN-FAIL` | Data port 48767 |
| `XFER-OFFER-OUT` / `XFER-OFFER-IN` | File offer sent/received |
| `XFER-OFFER-REJECT` | Bad name/size rejected |
| `XFER-ACCEPT` / `XFER-ACCEPT-FAIL` | Accept path |
| `XFER-REJECT` / `XFER-CANCEL` | Reject/cancel |
| `XFER-DATA-FAIL` / `XFER-COMPLETE` | Stream result |

## How to use when debugging

1. Reproduce the issue.
2. Open **Settings → Diagnostics** (or terminal log).
3. Note the **last ERROR/WARN codes** in order.
4. Jump to the code catalog above → matching module.
5. Search the repo for the code string (e.g. `TCP-PING-FAIL`) — each code is emitted at one logical node.

Example sleep/wake failure trail:

```text
DISC-PEER-EXPIRE → peers went offline
TCP-PING-FAIL → half-open write failed
TCP-RECONCILE-DROP → session dropped
TCP-DIAL-START → redial after peer returns
TCP-HELLO-OK → TCP-SESSION-UP
```

## Adding a new node

1. Add a variant to `LogicPoint` in `src-tauri/src/diagnostics.rs`.
2. Add `code()` + `area()` arms.
3. Call `diagnostics::info/warn/error(app, LogicPoint::…, "…")`.
4. Document the code in this file.
