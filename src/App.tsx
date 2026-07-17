import {
  ClipboardEvent,
  DragEvent as ReactDragEvent,
  FormEvent,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import "./App.css";

type AppInfo = {
  name: string;
  version: string;
  bundleId: string;
  platform: string;
  autoUpdate: boolean;
};

type AppPaths = {
  appDataDir: string;
  defaultSaveDir: string;
  configPath: string;
  historyNote: string;
};

type Identity = {
  deviceId: string;
  displayName: string;
  onboardingComplete: boolean;
  suggestedDisplayName: string;
};

type PeerInfo = {
  deviceId: string;
  displayName: string;
  os: string;
  controlPort: number;
  address: string;
  online: boolean;
  lastSeenMs: number;
};

type DiscoveryStatus = {
  running: boolean;
  port: number;
  controlPort: number;
  protocolVersion: number;
  lastError: string | null;
  peerCount: number;
  hint: string;
};

type ChatMessage = {
  id: string;
  peerId: string;
  direction: string;
  msgType: string;
  body: string;
  createdAt: number;
  status: string;
};

type FileCard = {
  fileId: string;
  name: string;
  size: number;
  mime: string;
  bytesDone: number;
  state: string;
  localPath?: string | null;
  sha256?: string | null;
  error?: string | null;
  resumeCapable?: boolean | null;
  /** Clipboard screenshot: receiver skips Accept. Image files still need Accept. */
  autoAccept?: boolean | null;
};

type TransferProgress = {
  fileId: string;
  messageId: string;
  peerId: string;
  bytesDone: number;
  size: number;
  state: string;
};

function parseFileCard(body: string): FileCard | null {
  try {
    return JSON.parse(body) as FileCard;
  } catch {
    return null;
  }
}

/** User-facing transfer state label. */
function fileStateLabel(state: string, autoAccept?: boolean | null): string {
  switch (state) {
    case "offered":
      return autoAccept ? "screenshot — auto receive" : "waiting for accept";
    case "accepted":
      return autoAccept ? "receiving screenshot…" : "starting…";
    case "transferring":
      return "transferring";
    case "interrupted":
      return "interrupted — can resume";
    case "completed":
      return "completed";
    case "failed":
      return "failed";
    case "cancelled":
      return "cancelled";
    case "rejected":
      return "rejected";
    default:
      return state;
  }
}

/** Map machine error codes to short readable text. */
function fileErrorLabel(error: string | null | undefined): string | null {
  if (!error) return null;
  const e = error.toLowerCase();
  if (e.includes("first_data_timeout")) {
    return "Sender did not start in time. Tap Resume to retry.";
  }
  if (e.includes("resume_timeout")) {
    return "Peer did not respond to resume (offline or older app). Tap Resume to retry.";
  }
  if (e.includes("offset_mismatch")) {
    return "Resume offset mismatch. Try Resume again or re-send the file.";
  }
  if (e.includes("source_changed") || e.includes("source_missing")) {
    return "Source file changed or is missing on sender. Ask them to re-send.";
  }
  if (e.includes("token_mismatch")) {
    return "Transfer token out of sync. Cancel and re-send.";
  }
  if (e.includes("busy")) {
    return "Sender is busy. Wait a moment and Resume.";
  }
  if (e.includes("unknown_file")) {
    return "Sender no longer has this transfer. Re-send the file.";
  }
  if (e.includes("sha256")) {
    return "Checksum failed — file may be corrupted. Re-send.";
  }
  if (e.includes("app_restart")) {
    return "App restarted; transfer can be resumed.";
  }
  if (e.includes("connection") || e.includes("eof") || e.includes("timed out") || e.includes("reset")) {
    return `Network interrupted: ${error}`;
  }
  return error;
}

function shortSha(sha: string | null | undefined): string | null {
  if (!sha || sha.length < 12) return sha ?? null;
  return `${sha.slice(0, 8)}…${sha.slice(-6)}`;
}

/** Encode binary for send_file_bytes (chunked to avoid call-stack limits). */
function bytesToBase64(bytes: Uint8Array): string {
  const chunk = 0x2000;
  let binary = "";
  for (let i = 0; i < bytes.length; i += chunk) {
    const sub = bytes.subarray(i, i + chunk);
    binary += String.fromCharCode.apply(null, sub as unknown as number[]);
  }
  return btoa(binary);
}

function screenshotFileName(mime: string): string {
  const stamp = new Date()
    .toISOString()
    .replace(/[:.]/g, "-")
    .replace("T", "_")
    .slice(0, 19);
  const ext =
    mime === "image/jpeg"
      ? "jpg"
      : mime === "image/gif"
        ? "gif"
        : mime === "image/webp"
          ? "webp"
          : "png";
  return `screenshot-${stamp}.${ext}`;
}

function looksLikeImageFile(file: FileCard): boolean {
  if (file.autoAccept) return true;
  const mime = (file.mime || "").toLowerCase();
  if (mime.startsWith("image/")) return true;
  const name = (file.name || "").toLowerCase();
  return /\.(png|jpe?g|jfif|gif|webp|bmp|tiff?|heic|heif|avif|ico)$/i.test(
    name,
  );
}

/** Inline chat preview for local screenshot / small image files. */
function FileImagePreview({
  path,
  cacheKey,
}: {
  path?: string | null;
  cacheKey: string;
}) {
  const [src, setSrc] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    if (!path) {
      setSrc(null);
      setFailed(false);
      return;
    }
    let cancelled = false;
    setFailed(false);
    setSrc(null);
    void (async () => {
      try {
        const url = await invoke<string>("read_local_image_preview", { path });
        if (!cancelled) setSrc(url);
      } catch {
        if (!cancelled) {
          setSrc(null);
          setFailed(true);
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [path, cacheKey]);

  if (!path) return null;
  if (failed) {
    return (
      <div className="file-preview-missing muted small">Preview unavailable</div>
    );
  }
  if (!src) {
    return <div className="file-preview-loading muted small">Loading preview…</div>;
  }
  return (
    <a
      className="file-preview-wrap"
      href={src}
      target="_blank"
      rel="noreferrer"
      title="Open preview"
      onClick={(e) => e.preventDefault()}
    >
      <img className="file-preview" src={src} alt="Screenshot preview" />
    </a>
  );
}

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(1)} MB`;
  return `${(n / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

/** Shared AudioContext — browsers/Tauri suspend until a user gesture; we resume every play. */
let sharedAudioCtx: AudioContext | null = null;

function getAudioCtx(): AudioContext | null {
  try {
    const AC =
      window.AudioContext ||
      (window as unknown as { webkitAudioContext: typeof AudioContext })
        .webkitAudioContext;
    if (!AC) return null;
    if (!sharedAudioCtx || sharedAudioCtx.state === "closed") {
      sharedAudioCtx = new AC();
    }
    return sharedAudioCtx;
  } catch {
    return null;
  }
}

/** Call on any click so later automatic message sounds are allowed. */
function unlockAudio() {
  const ctx = getAudioCtx();
  if (ctx && ctx.state === "suspended") {
    void ctx.resume();
  }
}

/** Web Audio beeps — primary path inside Tauri webview. */
function playWebSound(kind: string) {
  try {
    const ctx = getAudioCtx();
    if (!ctx) return;

    const startTones = () => {
      const now = ctx.currentTime;

      const tone = (
        freq: number,
        start: number,
        dur: number,
        vol = 0.22,
        type: OscillatorType = "sine",
      ) => {
        const o = ctx.createOscillator();
        const g = ctx.createGain();
        o.type = type;
        o.frequency.value = freq;
        g.gain.setValueAtTime(0.0001, now + start);
        g.gain.exponentialRampToValueAtTime(vol, now + start + 0.015);
        g.gain.exponentialRampToValueAtTime(0.0001, now + start + dur);
        o.connect(g);
        g.connect(ctx.destination);
        o.start(now + start);
        o.stop(now + start + dur + 0.02);
      };

      if (kind === "message") {
        tone(880, 0, 0.12);
        tone(1175, 0.1, 0.16);
      } else if (kind === "file_offer") {
        tone(523, 0, 0.15, 0.24, "triangle");
        tone(659, 0.16, 0.18, 0.24, "triangle");
        tone(784, 0.34, 0.2, 0.2, "triangle");
      } else if (kind === "file_done") {
        tone(523, 0, 0.1);
        tone(659, 0.1, 0.1);
        tone(784, 0.2, 0.12);
        tone(1046, 0.32, 0.22, 0.18);
      } else if (kind === "file_alert") {
        tone(220, 0, 0.25, 0.24, "square");
        tone(180, 0.2, 0.28, 0.2, "square");
      } else {
        tone(660, 0, 0.15);
      }
    };

    if (ctx.state === "suspended") {
      void ctx.resume().then(startTones).catch(() => {
        /* still suspended — user must click once in the app */
      });
    } else {
      startTones();
    }
  } catch {
    /* ignore audio errors */
  }
}

type SessionInfo = {
  peerId: string;
  displayName: string;
  address: string;
  connected: boolean;
};

type HistoryStats = {
  totalMessages: number;
};

type HistoryChanged = {
  scope: string;
  peerId: string | null;
  messageId: string | null;
  deleted: number;
};

type DiagEntry = {
  tsMs: number;
  code: string;
  area: string;
  level: "info" | "warn" | "error";
  message: string;
};

type Preferences = {
  soundEnabled: boolean;
  autoResumeTransfers?: boolean;
};

type Tab = "chat" | "settings";

function shortId(deviceId: string): string {
  return deviceId.replace(/-/g, "").slice(0, 8);
}

function invokeErrorMessage(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  try {
    return JSON.stringify(e);
  } catch {
    return String(e);
  }
}

function upsertMessage(
  list: ChatMessage[],
  msg: ChatMessage,
): ChatMessage[] {
  const idx = list.findIndex((m) => m.id === msg.id);
  if (idx === -1) {
    return [...list, msg].sort(
      (a, b) => a.createdAt - b.createdAt || a.id.localeCompare(b.id),
    );
  }
  const next = list.slice();
  next[idx] = msg;
  return next;
}

function App() {
  const [tab, setTab] = useState<Tab>("chat");
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [paths, setPaths] = useState<AppPaths | null>(null);
  const [identity, setIdentity] = useState<Identity | null>(null);
  const [peers, setPeers] = useState<PeerInfo[]>([]);
  const [sessions, setSessions] = useState<SessionInfo[]>([]);
  const [discovery, setDiscovery] = useState<DiscoveryStatus | null>(null);
  const [selectedPeerId, setSelectedPeerId] = useState<string | null>(null);
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [draft, setDraft] = useState("");
  const [sending, setSending] = useState(false);
  const [dragOver, setDragOver] = useState(false);
  const [error, setError] = useState<string | null>(null);
  /** Keep latest selection for Tauri drag-drop listener (avoids stale closures). */
  const selectedPeerIdRef = useRef<string | null>(null);
  const selectedConnectedRef = useRef(false);
  const sendingRef = useRef(false);
  const [nameDraft, setNameDraft] = useState("");
  const [settingsName, setSettingsName] = useState("");
  const [saving, setSaving] = useState(false);
  const [saveMsg, setSaveMsg] = useState<string | null>(null);
  const [historyStats, setHistoryStats] = useState<HistoryStats | null>(null);
  const [historyBusy, setHistoryBusy] = useState(false);
  const [diagnostics, setDiagnostics] = useState<DiagEntry[]>([]);
  const [diagFilter, setDiagFilter] = useState<"all" | "warn" | "error">("all");
  const [prefs, setPrefs] = useState<Preferences | null>(null);
  const [emptySince, setEmptySince] = useState<number | null>(null);
  const [now, setNow] = useState(Date.now());
  const messagesEndRef = useRef<HTMLDivElement | null>(null);

  async function refreshHistoryStats() {
    try {
      const s = await invoke<HistoryStats>("history_stats");
      setHistoryStats(s);
    } catch {
      /* ignore */
    }
  }

  async function refreshDiagnostics() {
    try {
      const rows = await invoke<DiagEntry[]>("list_diagnostics", { limit: 100 });
      setDiagnostics(rows);
    } catch {
      /* ignore */
    }
  }

  useEffect(() => {
    selectedPeerIdRef.current = selectedPeerId;
  }, [selectedPeerId]);

  useEffect(() => {
    const t = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(t);
  }, []);

  // Unlock Web Audio after any user gesture (required for auto message sounds).
  useEffect(() => {
    const unlock = () => unlockAudio();
    window.addEventListener("pointerdown", unlock);
    window.addEventListener("keydown", unlock);
    return () => {
      window.removeEventListener("pointerdown", unlock);
      window.removeEventListener("keydown", unlock);
    };
  }, []);

  useEffect(() => {
    let cancelled = false;
    const unlisteners: Array<() => void> = [];

    (async () => {
      try {
        const [appInfo, appPaths, id, peerList, status, sess, stats, diags, preferences] =
          await Promise.all([
            invoke<AppInfo>("get_app_info"),
            invoke<AppPaths>("get_app_paths"),
            invoke<Identity>("get_identity"),
            invoke<PeerInfo[]>("list_peers"),
            invoke<DiscoveryStatus>("get_discovery_status"),
            invoke<SessionInfo[]>("list_sessions"),
            invoke<HistoryStats>("history_stats"),
            invoke<DiagEntry[]>("list_diagnostics", { limit: 100 }),
            invoke<Preferences>("get_preferences"),
          ]);
        if (cancelled) return;
        setInfo(appInfo);
        setPaths(appPaths);
        setIdentity(id);
        setPeers(peerList);
        setDiscovery(status);
        setSessions(sess);
        setHistoryStats(stats);
        setDiagnostics(diags);
        setPrefs(preferences);
        setNameDraft(id.displayName || id.suggestedDisplayName || "");
        setSettingsName(id.displayName || "");

        unlisteners.push(
          await listen<PeerInfo[]>("peers-updated", (event) => {
            setPeers(event.payload);
          }),
        );
        unlisteners.push(
          await listen<DiscoveryStatus>("discovery-status", (event) => {
            setDiscovery(event.payload);
          }),
        );
        unlisteners.push(
          await listen<SessionInfo[]>("sessions-updated", (event) => {
            setSessions(event.payload);
          }),
        );
        unlisteners.push(
          await listen<ChatMessage>("message", (event) => {
            const msg = event.payload;
            if (msg.peerId === selectedPeerIdRef.current) {
              setMessages((prev) => upsertMessage(prev, msg));
            }
            void refreshHistoryStats();
          }),
        );
        unlisteners.push(
          await listen<HistoryChanged>("history-changed", (event) => {
            const ch = event.payload;
            const selected = selectedPeerIdRef.current;
            if (ch.scope === "all") {
              setMessages([]);
            } else if (
              ch.scope === "peer" &&
              selected &&
              ch.peerId === selected
            ) {
              setMessages([]);
            } else if (
              ch.scope === "message" &&
              selected &&
              ch.peerId === selected &&
              ch.messageId
            ) {
              setMessages((prev) =>
                prev.filter((m) => m.id !== ch.messageId),
              );
            }
            void refreshHistoryStats();
          }),
        );
        unlisteners.push(
          await listen<DiagEntry>("diagnostic", (event) => {
            setDiagnostics((prev) => [event.payload, ...prev].slice(0, 100));
          }),
        );
        unlisteners.push(
          await listen<TransferProgress>("transfer-progress", (event) => {
            const p = event.payload;
            if (p.peerId !== selectedPeerIdRef.current) return;
            setMessages((prev) =>
              prev.map((m) => {
                if (m.id !== p.messageId || m.msgType !== "file") return m;
                const card = parseFileCard(m.body);
                if (!card) return m;
                card.bytesDone = p.bytesDone;
                card.state = p.state;
                return { ...m, body: JSON.stringify(card) };
              }),
            );
          }),
        );
        // Primary sound path for notifications (Web Audio in the UI process)
        unlisteners.push(
          await listen<{ kind: string }>("play-sound", (event) => {
            playWebSound(event.payload.kind);
          }),
        );
      } catch (e) {
        if (!cancelled) setError(invokeErrorMessage(e));
      }
    })();

    return () => {
      cancelled = true;
      unlisteners.forEach((u) => u());
    };
  }, []);

  // Load history when selecting a peer.
  useEffect(() => {
    if (!selectedPeerId) {
      setMessages([]);
      return;
    }
    let cancelled = false;
    (async () => {
      try {
        const list = await invoke<ChatMessage[]>("list_messages", {
          peerId: selectedPeerId,
          limit: 500,
        });
        if (!cancelled) setMessages(list);
      } catch (e) {
        if (!cancelled) setError(invokeErrorMessage(e));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [selectedPeerId]);

  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages, selectedPeerId]);

  const onlinePeers = useMemo(
    () => peers.filter((p) => p.online),
    [peers],
  );

  const connectedIds = useMemo(
    () => new Set(sessions.filter((s) => s.connected).map((s) => s.peerId)),
    [sessions],
  );

  useEffect(() => {
    if (onlinePeers.length === 0) {
      setEmptySince((prev) => prev ?? Date.now());
    } else {
      setEmptySince(null);
    }
  }, [onlinePeers.length]);

  useEffect(() => {
    if (
      selectedPeerId &&
      !peers.some((p) => p.deviceId === selectedPeerId)
    ) {
      // Keep selection for history even if peer left discovery list.
    }
  }, [peers, selectedPeerId]);

  const showEmptyHelp =
    emptySince !== null && now - emptySince > 5000 && onlinePeers.length === 0;

  const selectedPeer =
    peers.find((p) => p.deviceId === selectedPeerId) ?? null;

  const selectedConnected = selectedPeerId
    ? connectedIds.has(selectedPeerId)
    : false;

  useEffect(() => {
    selectedConnectedRef.current = selectedConnected;
  }, [selectedConnected]);

  useEffect(() => {
    sendingRef.current = sending;
  }, [sending]);

  /** Offer one or more absolute paths (drag-drop). Sequential to avoid UI races. */
  const sendPaths = useCallback(async (paths: string[]) => {
    const peerId = selectedPeerIdRef.current;
    if (!peerId) {
      setError("Select a device first, then drop files.");
      return;
    }
    if (!selectedConnectedRef.current) {
      setError("Wait until the peer shows connected (green), then drop again.");
      return;
    }
    if (sendingRef.current) return;
    if (paths.length === 0) return;

    setSending(true);
    setError(null);
    try {
      for (const path of paths) {
        const msg = await invoke<ChatMessage>("send_file_from_path", {
          peerId,
          path,
        });
        setMessages((prev) => upsertMessage(prev, msg));
      }
      void refreshHistoryStats();
    } catch (err) {
      setError(invokeErrorMessage(err));
    } finally {
      setSending(false);
    }
  }, []);

  const sendClipboardImage = useCallback(
    async (blob: Blob) => {
      const peerId = selectedPeerIdRef.current;
      if (!peerId) {
        setError("Select a device first, then paste the screenshot.");
        return;
      }
      if (!selectedConnectedRef.current) {
        setError("Wait until the peer shows connected (green), then paste again.");
        return;
      }
      if (sendingRef.current) return;

      // Screenshots are tiny; refuse oversized clipboard blobs early.
      if (blob.size > 2 * 1024 * 1024) {
        setError("Clipboard image is larger than 2 MB — use File / drag-drop instead.");
        return;
      }

      setSending(true);
      setError(null);
      try {
        const mime = blob.type || "application/octet-stream";
        const buf = new Uint8Array(await blob.arrayBuffer());
        const base64Data = bytesToBase64(buf);
        const msg = await invoke<ChatMessage>("send_file_bytes", {
          peerId,
          fileName: screenshotFileName(mime),
          mime,
          base64Data,
          asScreenshotPaste: true, // only path that may auto-accept
        });
        setMessages((prev) => upsertMessage(prev, msg));
        void refreshHistoryStats();
      } catch (err) {
        setError(invokeErrorMessage(err));
      } finally {
        setSending(false);
      }
    },
    [],
  );

  /** Stage raw bytes as a normal file offer (always needs Accept). */
  const sendBytesAsFile = useCallback(async (blob: Blob, fileName: string) => {
    const peerId = selectedPeerIdRef.current;
    if (!peerId) {
      setError("Select a device first, then drop files.");
      return;
    }
    if (!selectedConnectedRef.current) {
      setError("Wait until the peer shows connected (green), then drop again.");
      return;
    }
    if (sendingRef.current) return;
    setSending(true);
    setError(null);
    try {
      const mime = blob.type || "application/octet-stream";
      const buf = new Uint8Array(await blob.arrayBuffer());
      const base64Data = bytesToBase64(buf);
      const msg = await invoke<ChatMessage>("send_file_bytes", {
        peerId,
        fileName,
        mime,
        base64Data,
        asScreenshotPaste: false, // drag/file path — receiver must Accept
      });
      setMessages((prev) => upsertMessage(prev, msg));
      void refreshHistoryStats();
    } catch (err) {
      setError(invokeErrorMessage(err));
    } finally {
      setSending(false);
    }
  }, []);

  /** Clipboard item looks like a screenshot/image even when type is empty. */
  function clipboardItemLooksLikeImage(item: DataTransferItem): boolean {
    if (item.kind !== "file") return false;
    const t = (item.type || "").toLowerCase();
    if (t.startsWith("image/")) return true;
    if (
      t === "" ||
      t === "application/octet-stream" ||
      t === "binary/octet-stream" ||
      t === "public.png" ||
      t === "public.jpeg" ||
      t === "public.tiff" ||
      t === "com.compuserve.gif" ||
      t === "org.webmproject.webp"
    ) {
      return true;
    }
    return false;
  }

  // Tauri native file drop → absolute paths (Finder → chat window).
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    (async () => {
      try {
        const webview = getCurrentWebview();
        unlisten = await webview.onDragDropEvent((event) => {
          if (cancelled) return;
          const payload = event.payload;
          if (payload.type === "enter" || payload.type === "over") {
            setDragOver(true);
          } else if (payload.type === "leave") {
            setDragOver(false);
          } else if (payload.type === "drop") {
            setDragOver(false);
            void sendPaths(payload.paths ?? []);
          }
        });
      } catch {
        /* webview API unavailable in plain browser preview */
      }
    })();
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [sendPaths]);

  function onComposerPaste(e: ClipboardEvent<HTMLElement>) {
    const items = e.clipboardData?.items;
    if (!items) return;
    for (let i = 0; i < items.length; i++) {
      const item = items[i];
      if (clipboardItemLooksLikeImage(item)) {
        const file = item.getAsFile();
        if (file) {
          e.preventDefault();
          void sendClipboardImage(file);
          return;
        }
      }
    }
  }

  /** HTML5 fallback drop: use Tauri File.path when present. */
  function onHtmlDragOver(e: ReactDragEvent) {
    if (!e.dataTransfer?.types?.includes("Files")) return;
    e.preventDefault();
    e.stopPropagation();
    setDragOver(true);
  }

  function onHtmlDragLeave(e: ReactDragEvent) {
    e.preventDefault();
    // only clear when leaving the panel (not entering a child)
    if (e.currentTarget === e.target) {
      setDragOver(false);
    }
  }

  function onHtmlDrop(e: ReactDragEvent) {
    e.preventDefault();
    e.stopPropagation();
    setDragOver(false);
    const files = e.dataTransfer?.files;
    if (!files || files.length === 0) return;
    const paths: string[] = [];
    for (let i = 0; i < files.length; i++) {
      const f = files[i] as File & { path?: string };
      // Tauri WKWebView exposes absolute path on dropped File.
      if (f.path) paths.push(f.path);
    }
    if (paths.length > 0) {
      void sendPaths(paths); // always requires Accept
      return;
    }
    // No OS path: still send as normal file (Accept required) — never as screenshot paste.
    const first = files[0];
    if (first) {
      void sendBytesAsFile(first, first.name || "dropped-file");
      return;
    }
    setError("Could not read dropped file. Try the File button, or drop from Finder.");
  }

  async function submitDisplayName(
    raw: string,
    from: "onboarding" | "settings",
  ) {
    setSaving(true);
    setError(null);
    setSaveMsg(null);
    try {
      const id = await invoke<Identity>("set_display_name", { name: raw });
      setIdentity(id);
      setSettingsName(id.displayName);
      setNameDraft(id.displayName);
      if (from === "settings") setSaveMsg("Display name saved.");
    } catch (e) {
      setError(invokeErrorMessage(e));
    } finally {
      setSaving(false);
    }
  }

  function onFirstRunSubmit(e: FormEvent) {
    e.preventDefault();
    void submitDisplayName(nameDraft, "onboarding");
  }

  function onSettingsSave(e: FormEvent) {
    e.preventDefault();
    void submitDisplayName(settingsName, "settings");
  }

  async function onSend(e: FormEvent) {
    e.preventDefault();
    if (!selectedPeerId || !draft.trim() || sending) return;
    setSending(true);
    setError(null);
    const body = draft;
    setDraft("");
    try {
      const msg = await invoke<ChatMessage>("send_text", {
        peerId: selectedPeerId,
        body,
      });
      setMessages((prev) => upsertMessage(prev, msg));
      void refreshHistoryStats();
    } catch (err) {
      setError(invokeErrorMessage(err));
      // Reload thread so failed/pending states show from DB events
      try {
        const list = await invoke<ChatMessage[]>("list_messages", {
          peerId: selectedPeerId,
          limit: 500,
        });
        setMessages(list);
      } catch {
        /* ignore */
      }
    } finally {
      setSending(false);
    }
  }

  async function onSendFile() {
    if (!selectedPeerId || sending) return;
    setSending(true);
    setError(null);
    try {
      const msg = await invoke<ChatMessage>("pick_and_send_file", {
        peerId: selectedPeerId,
      });
      setMessages((prev) => upsertMessage(prev, msg));
      void refreshHistoryStats();
    } catch (err) {
      const m = invokeErrorMessage(err);
      if (!m.toLowerCase().includes("no file selected")) {
        setError(m);
      }
    } finally {
      setSending(false);
    }
  }

  async function onAcceptFile(messageId: string) {
    if (!selectedPeerId) return;
    setError(null);
    try {
      await invoke("accept_file", {
        messageId,
        peerId: selectedPeerId,
      });
    } catch (e) {
      setError(invokeErrorMessage(e));
    }
  }

  async function onRejectFile(messageId: string) {
    if (!selectedPeerId) return;
    setError(null);
    try {
      await invoke("reject_file", {
        messageId,
        peerId: selectedPeerId,
      });
    } catch (e) {
      setError(invokeErrorMessage(e));
    }
  }

  async function onCancelFile(fileId: string) {
    if (!selectedPeerId) return;
    setError(null);
    try {
      await invoke("cancel_file", {
        fileId,
        peerId: selectedPeerId,
      });
    } catch (e) {
      setError(invokeErrorMessage(e));
    }
  }

  async function onResumeFile(messageId: string) {
    if (!selectedPeerId) return;
    setError(null);
    try {
      await invoke("resume_file", {
        messageId,
        peerId: selectedPeerId,
      });
    } catch (e) {
      setError(invokeErrorMessage(e));
    }
  }

  async function onOpenLocalPath(path: string) {
    setError(null);
    try {
      await invoke("open_local_path", { path });
    } catch (e) {
      setError(invokeErrorMessage(e));
    }
  }

  async function onRevealInFinder(path: string) {
    setError(null);
    try {
      await invoke("reveal_in_finder", { path });
    } catch (e) {
      setError(invokeErrorMessage(e));
    }
  }

  async function onDeleteMessage(messageId: string) {
    if (!selectedPeerId || historyBusy) return;
    setHistoryBusy(true);
    setError(null);
    try {
      await invoke<boolean>("delete_message", {
        messageId,
        peerId: selectedPeerId,
      });
      setMessages((prev) => prev.filter((m) => m.id !== messageId));
      void refreshHistoryStats();
    } catch (e) {
      setError(invokeErrorMessage(e));
    } finally {
      setHistoryBusy(false);
    }
  }

  async function onClearThread() {
    if (!selectedPeerId || historyBusy) return;
    const name = selectedPeer?.displayName || "this chat";
    const ok = window.confirm(
      `Clear all local messages with ${name}?\n\nThis only deletes history on this Mac. The other device is not notified. Received files on disk are not deleted.`,
    );
    if (!ok) return;
    setHistoryBusy(true);
    setError(null);
    try {
      await invoke<number>("clear_thread", { peerId: selectedPeerId });
      setMessages([]);
      void refreshHistoryStats();
    } catch (e) {
      setError(invokeErrorMessage(e));
    } finally {
      setHistoryBusy(false);
    }
  }

  async function onClearAllHistory() {
    if (historyBusy) return;
    const total = historyStats?.totalMessages ?? 0;
    const ok = window.confirm(
      `Delete ALL chat history on this Mac (${total} message${total === 1 ? "" : "s"})?\n\nIdentity and device id are kept. Uninstall also does not remove this folder unless you delete it manually. Received files on disk are not deleted.`,
    );
    if (!ok) return;
    const ok2 = window.confirm("Really clear everything? This cannot be undone.");
    if (!ok2) return;
    setHistoryBusy(true);
    setError(null);
    try {
      await invoke<number>("clear_all_history");
      setMessages([]);
      void refreshHistoryStats();
      setSaveMsg("All chat history cleared on this device.");
    } catch (e) {
      setError(invokeErrorMessage(e));
    } finally {
      setHistoryBusy(false);
    }
  }

  const needsOnboarding = identity !== null && !identity.onboardingComplete;

  return (
    <div className="app">
      <aside className="sidebar">
        <div className="brand">
          <div className="brand-mark">jc</div>
          <div>
            <div className="brand-name">jotainchatttttttt</div>
            <div className="brand-sub">LAN · Mac &amp; Windows · no cloud</div>
          </div>
        </div>

        <nav className="nav">
          <button
            type="button"
            className={tab === "chat" ? "nav-item active" : "nav-item"}
            onClick={() => setTab("chat")}
            disabled={needsOnboarding}
          >
            Chat
          </button>
          <button
            type="button"
            className={tab === "settings" ? "nav-item active" : "nav-item"}
            onClick={() => setTab("settings")}
            disabled={needsOnboarding}
          >
            Settings
          </button>
        </nav>

        <div className="peer-section">
          <div className="section-label">You</div>
          {identity?.onboardingComplete ? (
            <div className="you-card">
              <div className="you-name">{identity.displayName}</div>
              <div className="you-id mono">id {shortId(identity.deviceId)}</div>
            </div>
          ) : (
            <div className="empty-peers">Set your display name to continue.</div>
          )}

          <div className="section-label">
            Devices
            {discovery?.running ? (
              <span className="section-meta">
                {" "}
                · {onlinePeers.length} online
              </span>
            ) : (
              <span className="section-meta"> · discovery off</span>
            )}
          </div>

          {onlinePeers.length === 0 && peers.length === 0 ? (
            <div className="empty-peers">
              {showEmptyHelp ? (
                <>
                  <strong>No peers yet</strong>
                  <p>
                    {discovery?.lastError ||
                      "Another Mac must run jotainchatttttttt on the same Wi‑Fi."}
                  </p>
                  <ol className="help-steps">
                    <li>
                      Open jotainchatttttttt on the <strong>other device</strong>{" "}
                      (Mac or Windows, same Wi‑Fi — not guest / client-isolated).
                    </li>
                    <li>
                      <strong>Mac:</strong> Privacy &amp; Security →{" "}
                      <strong>Local Network</strong>.{" "}
                      <strong>Windows:</strong> Private network + allow firewall
                      when prompted.
                    </li>
                    <li>Allow firewall incoming connections if the OS asks.</li>
                    <li>
                      Wait until a peer appears, then wait for{" "}
                      <strong>connected (green)</strong> before chat or files.
                    </li>
                    <li>
                      Still empty? Settings → Diagnostics (codes like{" "}
                      <span className="mono">DISC-*</span>,{" "}
                      <span className="mono">TCP-*</span>).
                    </li>
                  </ol>
                </>
              ) : (
                "Looking for devices on your Wi‑Fi…"
              )}
            </div>
          ) : (
            <ul className="peer-list">
              {peers.map((p) => {
                const linked = connectedIds.has(p.deviceId);
                return (
                  <li key={p.deviceId}>
                    <button
                      type="button"
                      className={
                        selectedPeerId === p.deviceId
                          ? "peer-item active"
                          : "peer-item"
                      }
                      onClick={() => {
                        setSelectedPeerId(p.deviceId);
                        setTab("chat");
                        setError(null);
                      }}
                    >
                      <span
                        className={
                          linked
                            ? "dot online"
                            : p.online
                              ? "dot online dim"
                              : "dot offline"
                        }
                        aria-hidden
                      />
                      <span className="peer-text">
                        <span className="peer-name">
                          {p.displayName || "Unknown"}
                          {!p.online ? " (offline)" : linked ? "" : " · linking…"}
                        </span>
                        <span className="peer-meta mono">
                          {p.os ? `${p.os} · ` : ""}
                          {p.address} · {shortId(p.deviceId)}
                        </span>
                      </span>
                    </button>
                  </li>
                );
              })}
            </ul>
          )}
        </div>

        <div className="sidebar-footer">
          {info ? `v${info.version}` : "…"} · no auto-update
        </div>
      </aside>

      <main className="main">
        {error && !needsOnboarding && (
          <div className="banner error">{error}</div>
        )}

        {needsOnboarding && (
          <div className="modal-backdrop">
            <form className="modal" onSubmit={onFirstRunSubmit}>
              <h1>Welcome to jotainchatttttttt</h1>
              <p className="muted">
                Choose a display name for this device. Peers on your Wi‑Fi will
                see it. Your device id is permanent and stays on this computer
                even if you reinstall the app.
              </p>
              <label className="field">
                <span>Display name</span>
                <input
                  autoFocus
                  maxLength={32}
                  value={nameDraft}
                  onChange={(e) => setNameDraft(e.currentTarget.value)}
                  placeholder={identity?.suggestedDisplayName || "My Mac"}
                />
              </label>
              {identity && (
                <p className="mono small muted">
                  Device id: {identity.deviceId}
                </p>
              )}
              {error && <div className="banner error inline">{error}</div>}
              <button
                type="submit"
                className="primary"
                disabled={saving || !nameDraft.trim()}
              >
                {saving ? "Saving…" : "Continue"}
              </button>
            </form>
          </div>
        )}

        {tab === "chat" && !needsOnboarding && (
          <section
            className={`panel chat-panel${dragOver ? " drag-over" : ""}`}
            onPaste={onComposerPaste}
            onDragEnter={onHtmlDragOver}
            onDragOver={onHtmlDragOver}
            onDragLeave={onHtmlDragLeave}
            onDrop={onHtmlDrop}
          >
            {dragOver && (
              <div className="drop-overlay chat-drop-overlay" aria-hidden>
                Drop files here to send
              </div>
            )}
            <header className="panel-header chat-header">
              <div className="chat-header-text">
                <h1>
                  {selectedPeer
                    ? selectedPeer.displayName
                    : selectedPeerId
                      ? "Chat"
                      : "Messages"}
                </h1>
                <p className="muted">
                  {selectedPeer
                    ? `${selectedPeer.online ? "Online" : "Offline"}${selectedConnected ? " · connected" : selectedPeer.online ? " · connecting…" : ""} · ${selectedPeer.address} · id ${shortId(selectedPeer.deviceId)}`
                    : "1:1 only · same Wi‑Fi · no groups"}
                  {identity ? ` · you are ${identity.displayName}` : ""}
                </p>
              </div>
              {selectedPeerId && (
                <button
                  type="button"
                  className="ghost danger"
                  disabled={historyBusy || messages.length === 0}
                  onClick={() => void onClearThread()}
                  title="Clear this thread on this Mac only"
                >
                  Clear chat
                </button>
              )}
            </header>

            {!selectedPeerId ? (
              <div className="chat-placeholder">
                <p>Select a device on the left to start.</p>
                <p className="muted small">
                  Chat history stays in{" "}
                  <span className="mono">messages.db</span> on this Mac.
                  Replacing the app keeps history; uninstall does not wipe it.
                </p>
                <p className="muted small">
                  Files need <strong>Accept</strong> (except ⌘V screenshots
                  ≤2 MB). Drag files or use File when status is{" "}
                  <strong>connected</strong>.
                </p>
              </div>
            ) : (
              <div className="message-list">
                {messages.length === 0 ? (
                  <div className="chat-placeholder soft">
                    <p>No messages yet.</p>
                    <p className="muted small">
                      {selectedConnected
                        ? "Type a message, ⌘V a screenshot, or drop a file."
                        : "Wait for connected (green), then send."}
                    </p>
                  </div>
                ) : (
                  messages.map((m) => {
                    const file =
                      m.msgType === "file" ? parseFileCard(m.body) : null;
                    return (
                      <div
                        key={m.id}
                        className={
                          m.direction === "out"
                            ? "bubble-row out"
                            : "bubble-row in"
                        }
                      >
                        <div
                          className={
                            m.status === "failed" || file?.state === "failed"
                              ? "bubble failed"
                              : "bubble"
                          }
                        >
                          {file ? (
                            <div
                              className={
                                file.autoAccept
                                  ? "file-card file-card-screenshot"
                                  : "file-card"
                              }
                            >
                              <div className="file-title">
                                {file.autoAccept ? "🖼 " : "📎 "}
                                {file.name}
                              </div>
                              <div className="file-meta muted small">
                                {formatBytes(file.size)}
                                {file.mime ? ` · ${file.mime}` : ""}
                                {` · ${fileStateLabel(file.state, file.autoAccept)}`}
                              </div>
                              {/* Screenshot paste: show preview as soon as we have a local path (sender immediately; receiver after auto-save). */}
                              {file.localPath &&
                                (file.autoAccept || looksLikeImageFile(file)) &&
                                file.size > 0 &&
                                file.size <= 2 * 1024 * 1024 && (
                                  <FileImagePreview
                                    path={file.localPath}
                                    cacheKey={`${m.id}:${file.localPath}:${file.state}`}
                                  />
                                )}
                              {file.autoAccept &&
                                !file.localPath &&
                                file.state !== "failed" &&
                                file.state !== "rejected" &&
                                file.state !== "cancelled" && (
                                  <div className="file-preview-placeholder muted small">
                                    Screenshot · receiving…
                                  </div>
                                )}
                              {(file.state === "transferring" ||
                                file.state === "accepted" ||
                                file.state === "interrupted") &&
                                file.size > 0 && (
                                  <div className="progress-bar">
                                    <div
                                      className="progress-fill"
                                      style={{
                                        width: `${Math.min(
                                          100,
                                          (file.bytesDone / file.size) * 100,
                                        )}%`,
                                      }}
                                    />
                                  </div>
                                )}
                              {(file.bytesDone > 0 ||
                                file.state === "transferring") &&
                                (file.state === "transferring" ||
                                  file.state === "interrupted" ||
                                  file.state === "accepted") && (
                                  <div className="muted small">
                                    {formatBytes(file.bytesDone)} /{" "}
                                    {formatBytes(file.size)}
                                    {file.size > 0
                                      ? ` (${Math.min(
                                          100,
                                          Math.round(
                                            (file.bytesDone / file.size) * 100,
                                          ),
                                        )}%)`
                                      : ""}
                                  </div>
                                )}
                              {file.sha256 &&
                                (file.state === "offered" ||
                                  file.state === "completed") &&
                                !file.autoAccept && (
                                  <div className="mono small muted">
                                    SHA-256 {shortSha(file.sha256)}
                                    {file.state === "offered" ? " (offer)" : ""}
                                  </div>
                                )}
                              {file.localPath &&
                                (file.state === "completed" ||
                                  file.state === "interrupted") &&
                                !file.autoAccept && (
                                  <div className="mono small path">
                                    {file.localPath}
                                  </div>
                                )}
                              {fileErrorLabel(file.error) && (
                                <div className="small" style={{ color: "var(--danger)" }}>
                                  {fileErrorLabel(file.error)}
                                </div>
                              )}
                              <div className="file-actions">
                                {m.direction === "in" &&
                                  file.state === "offered" &&
                                  !file.autoAccept && (
                                    <>
                                      <button
                                        type="button"
                                        className="primary small-btn"
                                        onClick={() =>
                                          void onAcceptFile(m.id)
                                        }
                                      >
                                        Accept
                                      </button>
                                      <button
                                        type="button"
                                        className="ghost danger small-btn"
                                        onClick={() =>
                                          void onRejectFile(m.id)
                                        }
                                      >
                                        Reject
                                      </button>
                                    </>
                                  )}
                                {m.direction === "in" &&
                                  (file.state === "interrupted" ||
                                    file.state === "accepted") &&
                                  file.resumeCapable !== false && (
                                    <button
                                      type="button"
                                      className="primary small-btn"
                                      onClick={() => void onResumeFile(m.id)}
                                    >
                                      Resume
                                    </button>
                                  )}
                                {file.localPath &&
                                  file.state !== "failed" &&
                                  file.state !== "rejected" &&
                                  file.state !== "cancelled" && (
                                    <>
                                      <button
                                        type="button"
                                        className="primary small-btn"
                                        onClick={() =>
                                          void onOpenLocalPath(file.localPath!)
                                        }
                                        title="Open with the default macOS app"
                                      >
                                        Open
                                      </button>
                                      <button
                                        type="button"
                                        className="ghost small-btn"
                                        onClick={() =>
                                          void onRevealInFinder(file.localPath!)
                                        }
                                        title="Show this file in Finder / Explorer"
                                      >
                                        Show in folder
                                      </button>
                                    </>
                                  )}
                                {(file.state === "transferring" ||
                                  file.state === "offered" ||
                                  file.state === "accepted" ||
                                  file.state === "interrupted") && (
                                  <button
                                    type="button"
                                    className="ghost small-btn"
                                    onClick={() =>
                                      void onCancelFile(file.fileId)
                                    }
                                  >
                                    Cancel
                                  </button>
                                )}
                              </div>
                            </div>
                          ) : (
                            <div className="bubble-body">{m.body}</div>
                          )}
                          <div className="bubble-meta">
                            <span>
                              {new Date(m.createdAt).toLocaleTimeString()}
                              {m.direction === "out" && !file
                                ? ` · ${m.status}`
                                : ""}
                            </span>
                            <button
                              type="button"
                              className="msg-delete"
                              disabled={historyBusy}
                              onClick={() => void onDeleteMessage(m.id)}
                              title="Delete from this Mac only"
                            >
                              Delete
                            </button>
                          </div>
                        </div>
                      </div>
                    );
                  })
                )}
                <div ref={messagesEndRef} />
              </div>
            )}

            <div className={`composer-wrap${dragOver ? " drag-over" : ""}`}>
              {dragOver && (
                <div className="drop-overlay" aria-hidden>
                  Drop files to send
                </div>
              )}
              <form
                className={
                  selectedPeerId ? "composer" : "composer disabled"
                }
                onSubmit={onSend}
              >
                <button
                  type="button"
                  className="ghost attach-btn"
                  disabled={!selectedPeerId || sending || !selectedConnected}
                  onClick={() => void onSendFile()}
                  title={
                    selectedConnected
                      ? "Send file — receiver must Accept. Drag files here, or ⌘V a screenshot (≤2 MB auto-receives)."
                      : "Wait until peer shows connected (green)"
                  }
                >
                  File
                </button>
                <input
                  type="text"
                  placeholder={
                    selectedPeerId
                      ? selectedConnected
                        ? "Message… · ⌘V screenshot · drop file"
                        : "Wait for connected (green)…"
                      : "Select a device first"
                  }
                  value={draft}
                  disabled={!selectedPeerId || sending}
                  onChange={(e) => setDraft(e.currentTarget.value)}
                  onPaste={onComposerPaste}
                  maxLength={16000}
                />
                <button
                  type="submit"
                  disabled={
                    !selectedPeerId || sending || !draft.trim()
                  }
                >
                  {sending ? "…" : "Send"}
                </button>
              </form>
            </div>
          </section>
        )}

        {tab === "settings" && !needsOnboarding && (
          <section className="panel settings-panel">
            <header className="panel-header">
              <h1>Settings</h1>
              <p className="muted">
                Identity, discovery, sessions, paths, and product rules.
              </p>
            </header>

            <div className="settings-grid">
              <div className="card">
                <h2>Identity</h2>
                <form className="identity-form" onSubmit={onSettingsSave}>
                  <label className="field">
                    <span>Display name</span>
                    <input
                      maxLength={32}
                      value={settingsName}
                      onChange={(e) => {
                        setSettingsName(e.currentTarget.value);
                        setSaveMsg(null);
                      }}
                    />
                  </label>
                  <div className="form-row">
                    <button
                      type="submit"
                      className="primary"
                      disabled={
                        saving ||
                        !settingsName.trim() ||
                        settingsName.trim() === identity?.displayName
                      }
                    >
                      {saving ? "Saving…" : "Save name"}
                    </button>
                    {saveMsg && <span className="ok">{saveMsg}</span>}
                  </div>
                </form>
                {identity && (
                  <dl className="tight">
                    <dt>Device id</dt>
                    <dd className="mono path">{identity.deviceId}</dd>
                    <dt>Stable?</dt>
                    <dd>Yes — not changed on reinstall (same data folder)</dd>
                  </dl>
                )}
              </div>

              <div className="card">
                <h2>LAN discovery</h2>
                {discovery ? (
                  <>
                    <dl>
                      <dt>Status</dt>
                      <dd>
                        {discovery.running ? "Running" : "Stopped"}
                        {discovery.lastError
                          ? ` · error: ${discovery.lastError}`
                          : ""}
                      </dd>
                      <dt>UDP discovery</dt>
                      <dd className="mono">{discovery.port}</dd>
                      <dt>TCP control</dt>
                      <dd className="mono">{discovery.controlPort}</dd>
                      <dt>TCP file data</dt>
                      <dd className="mono">48767</dd>
                      <dt>Protocol</dt>
                      <dd>v{discovery.protocolVersion}</dd>
                      <dt>Online peers</dt>
                      <dd>{discovery.peerCount}</dd>
                      <dt>TCP sessions</dt>
                      <dd>{sessions.length}</dd>
                    </dl>
                    <p className="note">{discovery.hint}</p>
                  </>
                ) : (
                  <p className="muted">Loading…</p>
                )}
              </div>

              <div className="card">
                <h2>LAN help</h2>
                <ul className="rules">
                  <li>
                    Both devices (Mac and/or Windows) open this app on the same
                    Wi‑Fi.
                  </li>
                  <li>
                    <strong>Mac:</strong> Local Network permission.{" "}
                    <strong>Windows:</strong> Private network profile + firewall
                    allow for this app.
                  </li>
                  <li>Avoid guest networks with client isolation.</li>
                  <li>
                    After sleep/wake, wait a few seconds until status is{" "}
                    <strong>connected</strong>.
                  </li>
                  <li>
                    <strong>Files / drag / File button:</strong> receiver must{" "}
                    <strong>Accept</strong>.
                  </li>
                  <li>
                    <strong>⌘V screenshot</strong> ≤2 MB: auto-receives; shows
                    preview; Open / Show in Finder when saved.
                  </li>
                  <li>
                    Use <strong>Diagnostics</strong> below if something fails.
                  </li>
                </ul>
              </div>

              <div className="card">
                <h2>Sounds</h2>
                <p className="muted small">
                  System alert sounds for incoming text, file offers, and
                  finished transfers.
                </p>
                <label className="toggle-row">
                  <input
                    type="checkbox"
                    checked={prefs?.soundEnabled ?? true}
                    onChange={(e) => {
                      const enabled = e.currentTarget.checked;
                      void (async () => {
                        try {
                          const p = await invoke<Preferences>(
                            "set_sound_enabled",
                            { enabled },
                          );
                          setPrefs(p);
                        } catch (err) {
                          setError(invokeErrorMessage(err));
                        }
                      })();
                    }}
                  />
                  <span>Enable notification sounds</span>
                </label>
                <div className="form-row" style={{ marginTop: 10, flexWrap: "wrap" }}>
                  <button
                    type="button"
                    className="ghost small-btn"
                    onClick={() => {
                      playWebSound("message");
                      void invoke("preview_sound", { kind: "message" });
                    }}
                  >
                    Preview message
                  </button>
                  <button
                    type="button"
                    className="ghost small-btn"
                    onClick={() => {
                      playWebSound("file_offer");
                      void invoke("preview_sound", { kind: "file_offer" });
                    }}
                  >
                    Preview file offer
                  </button>
                  <button
                    type="button"
                    className="ghost small-btn"
                    onClick={() => {
                      playWebSound("file_done");
                      void invoke("preview_sound", { kind: "file_done" });
                    }}
                  >
                    Preview file done
                  </button>
                </div>
              </div>

              <div className="card">
                <h2>File transfers</h2>
                <p className="muted small">
                  When a peer reconnects, automatically resume interrupted
                  downloads. You can still tap Resume manually. Large sends
                  compute a checksum before the offer (progress may pause
                  briefly).
                </p>
                <label className="toggle-row">
                  <input
                    type="checkbox"
                    checked={prefs?.autoResumeTransfers ?? true}
                    onChange={(e) => {
                      const enabled = e.currentTarget.checked;
                      void (async () => {
                        try {
                          const p = await invoke<Preferences>(
                            "set_auto_resume_transfers",
                            { enabled },
                          );
                          setPrefs(p);
                        } catch (err) {
                          setError(invokeErrorMessage(err));
                        }
                      })();
                    }}
                  />
                  <span>Auto-resume transfers when peer is online</span>
                </label>
              </div>

              <div className="card card-wide">
                <h2>About / how to update</h2>
                {info ? (
                  <dl>
                    <dt>Name</dt>
                    <dd>{info.name}</dd>
                    <dt>Version</dt>
                    <dd className="mono">{info.version}</dd>
                    <dt>Bundle ID</dt>
                    <dd className="mono">{info.bundleId}</dd>
                    <dt>Platform</dt>
                    <dd>
                      {info.platform} · LAN peers: macOS &amp; Windows (same
                      protocol)
                    </dd>
                    <dt>Auto-update</dt>
                    <dd>
                      {info.autoUpdate
                        ? "enabled"
                        : "Off — install new .app / zip manually"}
                    </dd>
                  </dl>
                ) : (
                  <p className="muted">Loading…</p>
                )}
                <h3 className="card-subhead">How to update</h3>
                <ol className="help-steps">
                  <li>
                    Get a new zip (e.g.{" "}
                    <span className="mono">
                      jotainchatttttttt-macos-arm64-v…
                    </span>
                    ).
                  </li>
                  <li>Quit jotainchatttttttt completely.</li>
                  <li>
                    Replace the old <span className="mono">.app</span> (Desktop
                    or Applications). Unzip → Open-Me-First or right-click →
                    Open.
                  </li>
                  <li>
                    <strong>Chat history is kept</strong> — same folder:{" "}
                    <span className="mono">
                      ~/Library/Application Support/com.jotain.jotainchatttttttt/
                    </span>
                  </li>
                </ol>
                <h3 className="card-subhead">Data on this Mac</h3>
                {paths ? (
                  <dl>
                    <dt>Config + history DB</dt>
                    <dd className="mono path">{paths.appDataDir}</dd>
                    <dt>Received files</dt>
                    <dd className="mono path">{paths.defaultSaveDir}</dd>
                    <dt>Note</dt>
                    <dd className="small muted">{paths.historyNote}</dd>
                  </dl>
                ) : (
                  <p className="muted">Loading paths…</p>
                )}
                <h3 className="card-subhead">LAN ports</h3>
                <dl>
                  <dt>Discovery</dt>
                  <dd className="mono">UDP 48765</dd>
                  <dt>Chat / file signaling</dt>
                  <dd className="mono">TCP 48766</dd>
                  <dt>File data</dt>
                  <dd className="mono">TCP 48767</dd>
                </dl>
                <p className="note">
                  No cloud account. Traffic stays on your Wi‑Fi. Gatekeeper:
                  right-click → Open if macOS blocks an unsigned build.
                </p>
              </div>

              <div className="card card-wide">
                <h2>Diagnostics (logic nodes)</h2>
                <p className="muted small">
                  Stable codes for every critical path. Match stderr{" "}
                  <span className="mono">[JC][CODE][LEVEL]</span> or rows below
                  when debugging. See <span className="mono">docs/diagnostics.md</span>.
                </p>
                <div className="form-row" style={{ marginTop: 8, flexWrap: "wrap" }}>
                  <button
                    type="button"
                    className={diagFilter === "all" ? "ghost active-filter" : "ghost"}
                    onClick={() => setDiagFilter("all")}
                  >
                    All
                  </button>
                  <button
                    type="button"
                    className={diagFilter === "warn" ? "ghost active-filter" : "ghost"}
                    onClick={() => setDiagFilter("warn")}
                  >
                    Warn+
                  </button>
                  <button
                    type="button"
                    className={diagFilter === "error" ? "ghost active-filter" : "ghost"}
                    onClick={() => setDiagFilter("error")}
                  >
                    Error
                  </button>
                  <button
                    type="button"
                    className="ghost"
                    onClick={() => void refreshDiagnostics()}
                  >
                    Refresh
                  </button>
                  <button
                    type="button"
                    className="ghost danger"
                    onClick={() => {
                      void (async () => {
                        await invoke("clear_diagnostics");
                        setDiagnostics([]);
                      })();
                    }}
                  >
                    Clear log
                  </button>
                </div>
                <div className="diag-list">
                  {diagnostics
                    .filter((d) => {
                      if (diagFilter === "all") return true;
                      if (diagFilter === "warn")
                        return d.level === "warn" || d.level === "error";
                      return d.level === "error";
                    })
                    .slice(0, 80)
                    .map((d, i) => (
                      <div
                        key={`${d.tsMs}-${d.code}-${i}`}
                        className={`diag-row level-${d.level}`}
                      >
                        <span className="diag-code mono">{d.code}</span>
                        <span className="diag-area">{d.area}</span>
                        <span className="diag-msg">{d.message}</span>
                        <span className="diag-time mono">
                          {new Date(d.tsMs).toLocaleTimeString()}
                        </span>
                      </div>
                    ))}
                  {diagnostics.length === 0 && (
                    <p className="muted small">No diagnostics yet.</p>
                  )}
                </div>
              </div>

              <div className="card">
                <h2>Chat history</h2>
                <dl>
                  <dt>Messages on this Mac</dt>
                  <dd>
                    {historyStats
                      ? historyStats.totalMessages
                      : "…"}
                  </dd>
                </dl>
                <p className="note">
                  History is stored only on this device and is never auto-deleted.
                  Removing the app does <strong>not</strong> wipe it — use the
                  button below, or delete the data folder manually.
                </p>
                <div className="form-row" style={{ marginTop: 12 }}>
                  <button
                    type="button"
                    className="ghost danger"
                    disabled={
                      historyBusy ||
                      !historyStats ||
                      historyStats.totalMessages === 0
                    }
                    onClick={() => void onClearAllHistory()}
                  >
                    {historyBusy ? "Working…" : "Clear all history"}
                  </button>
                </div>
              </div>

              <div className="card">
                <h2>Data locations</h2>
                {paths ? (
                  <>
                    <dl>
                      <dt>App data / history</dt>
                      <dd className="mono path">{paths.appDataDir}</dd>
                      <dt>Config file</dt>
                      <dd className="mono path">{paths.configPath}</dd>
                      <dt>Messages DB</dt>
                      <dd className="mono path">
                        {paths.appDataDir.replace(/\/?$/, "/") + "messages.db"}
                      </dd>
                      <dt>Default save dir</dt>
                      <dd className="mono path">{paths.defaultSaveDir}</dd>
                    </dl>
                    <p className="note">{paths.historyNote}</p>
                  </>
                ) : (
                  <p className="muted">Loading…</p>
                )}
              </div>

              <div className="card">
                <h2>Product rules (v1)</h2>
                <ul className="rules">
                  <li>No automatic updates (replace the .app manually)</li>
                  <li>
                    File / drag receive requires Accept; ⌘V screenshot ≤2 MB
                    auto-receives
                  </li>
                  <li>Discovery is enough to chat (no pairing)</li>
                  <li>No group chat</li>
                  <li>
                    History kept until you delete it; uninstall does not wipe
                    data
                  </li>
                  <li>macOS + Windows (same LAN wire protocol)</li>
                </ul>
              </div>
            </div>
          </section>
        )}
      </main>
    </div>
  );
}

export default App;
