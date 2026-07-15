# jotainchatttttttt — v1 存档 / 交接文档

**存档日期：** 2026-07-14  
**状态：** v1 完成，已可分发使用  
**下次增强：** 从本文 + 设计文档恢复上下文，无需重做 brainstorm

---

## 1. 项目在哪

| 项 | 路径 |
|----|------|
| 源码根目录 | `/Users/xiatong/Documents/jotainchatttttttt` |
| 设计 | `docs/2026-07-14-design.md` |
| 诊断码表 | `docs/diagnostics.md` |
| macOS 安装说明 | `docs/setup-macos.md` |
| QA 清单 | `docs/QA-checklist.md` |
| 本文（存档） | `docs/ARCHIVE-v1-handoff.md` |
| 发布包（推荐） | `dist/jotainchatttttttt.app` + `dist/jotainchatttttttt-macos-arm64-v0.1.0-*.zip` |
| 打包命令 | `npm run package:mac`（签名修复 + zip） |

---

## 2. 产品是什么（一句话）

**局域网 Mac ↔ Mac 桌面聊天 + 文件传输**：经 Wi‑Fi 路由器互通，无账号、无云端、**无自动更新**。

---

## 3. 已锁定产品决策（不要在增强时静默推翻）

| # | 决策 |
|---|------|
| 1 | **无自动更新** — 手动安装/替换 |
| 2 | **文件接收默认需确认** Accept |
| 3 | **发现即可聊** — v1 无配对码 |
| 4 | **不做群聊**（v1） |
| 5 | **历史本机永久保留** — 卸载不删 Application Support；需应用内或手动删 |
| 6 | 产品名：**jotainchatttttttt** |
| 7 | 平台：**仅 macOS**（Windows 曾从计划中删除） |

---

## 4. 技术栈

- **Tauri 2** + **Rust** + **React/TypeScript** + **SQLite** (rusqlite bundled)
- Bundle ID: `com.jotain.jotainchatttttttt`
- 单实例：`tauri-plugin-single-instance`
- 文件选择：`rfd`；哈希：`sha2`

---

## 5. 实现进度（PR1–PR7 全部完成）

| PR | 内容 | 状态 |
|----|------|------|
| PR1 | Tauri 脚手架、无 updater、单实例 | ✅ |
| PR2 | 稳定 `device_id`、显示名、首次 onboarding、`config.json` | ✅ |
| PR3 | UDP 发现、设备列表、Local Network 空状态 | ✅ |
| PR4 | TCP 1:1 文本、拨号仲裁、SQLite 历史 | ✅ |
| PR5 | 删消息 / 清会话 / 清全部 | ✅ |
| PR6 | 文件 offer→Accept/Reject→数据流+SHA-256 | ✅ |
| PR7 | 打包、文档、Settings 帮助/About、QA 清单 | ✅ |

额外：全链路 **诊断码**（Settings → Diagnostics / stderr `[JC][CODE][LEVEL]`）

---

## 6. 架构摘要

```
UI (React)
  ↕ Tauri IPC / events
Rust core
  · discovery   UDP 48765
  · session     TCP 48766  (hello, text, file signaling)
  · transfer    TCP 48767  (file bytes after Accept)
  · db          messages.db
  · config      config.json
  · diagnostics ring buffer
```

### 端口

| 角色 | 端口 |
|------|------|
| 发现 | UDP **48765** |
| 控制（聊天+文件信令） | TCP **48766** |
| 文件数据 | TCP **48767** |

### 关键协议行为

- 发现：UDP JSON announce；`device_id` 自过滤；TTL 约 6s offline / 30s 移除  
- 会话：UUID 更小一方拨号；`Arc::ptr_eq` 清理；休眠后 ping 失败 / peer offline 丢弃假 connected  
- 文件：控制面 FileOffer → Accept/Reject/Cancel；**未 Accept 拒绝数据面**；`*.partial` + SHA-256  

### 事件

`peers-updated` · `discovery-status` · `sessions-updated` · `message` · `history-changed` · `transfer-progress` · `diagnostic`

### 命令（Rust）

身份/路径 · 发现 · 消息 · 历史删除 · 文件 pick/accept/reject/cancel · diagnostics list/clear  

---

## 7. 数据位置（卸载不删）

```text
~/Library/Application Support/com.jotain.jotainchatttttttt/
  config.json      # deviceId, displayName, onboarding
  messages.db      # 聊天 + 文件卡片 JSON

~/Downloads/jotainchatttttttt/   # 默认接收目录
```

---

## 8. 常用命令

```bash
cd ~/Documents/jotainchatttttttt

# 开发
npm install
npm run tauri:dev

# 测试
cd src-tauri && cargo test
cd .. && npm run build

# 发布（仅 .app；DMG 在本环境 create-dmg 曾失败，故 targets 只含 app）
npm run tauri:build
# → src-tauri/target/release/bundle/macos/jotainchatttttttt.app
```

分发：拷贝 `.app`；未签名时右键 → 打开。

---

## 9. 已修过的重要坑（再犯时先查 diagnostics）

| 场景 | 处理 |
|------|------|
| 休眠假 connected / 无法 redial | reconcile + ping 失败 drop session |
| dialing 永久卡住 | spawn 失败 clear dialing |
| config 损坏起不来 | quarantine + 重建 |
| 历史 LIMIT 取最旧 | DESC 再 reverse |
| 文件未 Accept 可传数据 | `accepted` 标志强制确认 |
| Accept 后 IP 过期 | 推送前 re-resolve 地址 |
| 读超时误断 TCP | FrameError + ErrorKind TimedOut/WouldBlock |

诊断码表：`docs/diagnostics.md`（含 `XFER-*`）。

---

## 10. 已知限制 / 下次可增强方向

**v1 不做 / 已知限制：**

- 无群聊、无配对码、无 E2E TLS  
- 无跨公网  
- 无自动更新  
- 单机不能开双实例互测  
- DMG 打包在本机曾失败（现只出 `.app`）  
- 未 Developer ID 签名  

**若下次增强，候选（按需选）：**

1. Developer ID 签名 + 修 DMG  
2. 可选配对码 / 房间口令  
3. 群聊  
4. Windows 客户端（同一 wire protocol）  
5. TLS / TOFU  
6. 文件夹发送、断点续传  
7. 离线消息队列（仍无云）  
8. Offer TTL、诊断落盘  
9. 系统睡眠唤醒 API 主动刷新  

---

## 11. 下次如何继续（给未来的自己 / Agent）

1. 打开仓库：`/Users/xiatong/Documents/jotainchatttttttt`  
2. 读本文 + `docs/2026-07-14-design.md` + `docs/diagnostics.md`  
3. 说明要做的增强点（从上表选或新需求）  
4. **不要**默认推翻 §3 锁定决策，除非用户明确改决策  
5. 新逻辑节点：扩展 `LogicPoint` + 更新 `docs/diagnostics.md`  
6. 改协议时 bump `PROTOCOL_VERSION` 并考虑兼容  

**会话提示词示例：**

```text
继续 jotainchatttttttt。项目在 ~/Documents/jotainchatttttttt。
先读 docs/ARCHIVE-v1-handoff.md 和 design。
v1 已分发；本次要做：<增强描述>。
保持无自动更新、文件默认确认、Mac only，除非我另说。
```

---

## 12. 文件树（核心）

```text
jotainchatttttttt/
  README.md
  package.json
  src/                     # React UI
  src-tauri/
    tauri.conf.json        # targets: ["app"] only
    Info.plist             # NSLocalNetworkUsageDescription
    src/
      lib.rs
      config.rs
      db.rs
      diagnostics.rs
      discovery.rs
      fsutil.rs
      state.rs
      net/
        frame.rs
        protocol.rs
        session.rs
        transfer.rs
  docs/
    2026-07-14-design.md
    diagnostics.md
    setup-macos.md
    QA-checklist.md
    ARCHIVE-v1-handoff.md   ← 本文件
```

---

## 13. 存档结论

- **v1 功能闭环可用**，可拷贝 `.app` 分发。  
- **设计决策与实现进度已写入本仓库**，不依赖某次聊天记录。  
- 下次增强：打开本文件即可接上。  
