# Group chat (v0.2.1-group-chat)

## What it is

LAN **text-only** group chat on top of existing 1:1 TCP sessions (mesh fan-out).

| Allowed | Forbidden |
|---------|-----------|
| Create group | File button |
| Join with **join code** | Drag-and-drop files |
| Send / receive text | Screenshot paste |
| Leave group | Any document / image transfer in group |

## Create a group

1. Sidebar → **New group** → name → Create.  
2. You become the first member.  
3. Share the **6-character join code** (shown on the group row and header).

## Join (verify membership)

1. At least one **current member** must be on the LAN and **connected (green)** to someone in the group mesh (typically connected to you or to a mutual peer — the joiner floods the request to **all** of their connected peers).  
2. Sidebar → **Join code** → enter the code.  
3. A member who knows that code verifies it and sends back the roster (`groupJoinOk`).  
4. Wrong code / no member online → join fails after a few seconds.

**Verification = correct join code + an online member who already has the group.**

## Quit chatting (leave)

1. Open the group.  
2. **Leave group**.  
3. Other members get a leave / roster update.  
4. You stop receiving group messages. **Local history** remains until **Clear chat**.

## Push history to a newcomer (group creator only)

Live messages are not stored in the cloud. If someone was offline or joined late, the **group creator** can push text history:

1. Creator opens the group → **Share history…**  
2. Choose **from date** (inclusive) and **member** (must be in the group and **connected**).  
3. Newcomer sees a banner: **Accept** or **Decline**.  
4. On **Accept**, history chunks are sent and merged into their local group chat (`INSERT OR IGNORE` by message id).  
5. Cap: **2000** text messages per offer. **No files** in history push.

## How messages travel

Each group text is sent as `groupText` over **existing 1:1 control sessions** to every other member device id. There is no separate group server.

## History

Stored in `messages.db` with `peer_id = g:{groupId}` and `msg_type = gtext`.  
Compatible with the same DB file as 1:1 history.

## Requirements

- Peers need **v0.2.1+** to understand group wire messages. Older apps ignore unknown frames (join may fail until they upgrade).
