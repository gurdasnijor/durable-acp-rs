import { useState, useRef, useEffect } from "react";
import * as acp from "@agentclientprotocol/sdk";

const STATE_URL = `/api/v1/state`;

export function App() {
  return (
    <div style={{ display: "flex", height: "100vh" }}>
      <div style={{ flex: 2, display: "flex", flexDirection: "column", borderRight: "1px solid #333" }}>
        <SessionPanel />
      </div>
      <div style={{ flex: 1, display: "flex", flexDirection: "column" }}>
        <StatePanel />
        <FilePanel />
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// WebSocket → acp.Stream bridge (same pattern as Flamecast)
// ---------------------------------------------------------------------------

function createWebSocketStream(ws: WebSocket): acp.Stream {
  return {
    readable: new ReadableStream({
      start(controller) {
        ws.addEventListener("message", (event) => {
          toText(event.data)
            .then((text) => controller.enqueue(JSON.parse(text)))
            .catch((error) => controller.error(error));
        });
        ws.addEventListener("close", () => controller.close(), { once: true });
        ws.addEventListener("error", () => controller.error(new Error("WS error")), { once: true });
      },
    }),
    writable: new WritableStream({
      write(message) { ws.send(JSON.stringify(message)); },
      close() { ws.close(); },
      abort() { ws.close(); },
    }),
  };
}

async function toText(data: Blob | ArrayBuffer | string): Promise<string> {
  if (typeof data === "string") return data;
  if (data instanceof Blob) return await data.text();
  return new TextDecoder().decode(data);
}

// ---------------------------------------------------------------------------
// ACP Session Panel
// ---------------------------------------------------------------------------

type SessionEvent = { type: string; data: any; timestamp: string };

function SessionPanel() {
  const [input, setInput] = useState("");
  const [events, setEvents] = useState<SessionEvent[]>([]);
  const [status, setStatus] = useState<"disconnected" | "connecting" | "connected">("disconnected");
  const [sessionId, setSessionId] = useState<string | null>(null);
  const [pendingPerm, setPendingPerm] = useState<any>(null);
  const connectionRef = useRef<acp.ClientSideConnection | null>(null);
  const sessionIdRef = useRef<string | null>(null);
  const permResolverRef = useRef<((v: any) => void) | null>(null);
  const messagesEnd = useRef<HTMLDivElement>(null);

  useEffect(() => {
    messagesEnd.current?.scrollIntoView({ behavior: "smooth" });
  }, [events.length]);

  const connect = () => {
    const wsUrl = `ws://${window.location.host}/acp`;
    setStatus("connecting");

    const ws = new WebSocket(wsUrl);
    ws.onopen = async () => {
      try {
        const connection = new acp.ClientSideConnection(
          () => ({
            async requestPermission(params: any) {
              setPendingPerm(params);
              return new Promise((resolve) => { permResolverRef.current = resolve; });
            },
            async sessionUpdate(params: any) {
              console.log("sessionUpdate:", JSON.stringify(params));
              setEvents((prev) => [...prev, {
                type: "session_update",
                data: params.update || params,
                timestamp: new Date().toISOString(),
              }]);
            },
            async readTextFile() { return { content: "" }; },
            async writeTextFile() { return {}; },
          }),
          createWebSocketStream(ws),
        );

        connectionRef.current = connection;
        await connection.initialize({
          protocolVersion: acp.PROTOCOL_VERSION,
          clientCapabilities: { fs: { readTextFile: false } },
        });
        const session = await connection.newSession({ cwd: "/", mcpServers: [] });
        sessionIdRef.current = session.sessionId;
        setSessionId(session.sessionId);
        setStatus("connected");
      } catch (err: any) {
        console.error("ACP init failed:", err);
        setStatus("disconnected");
      }
    };
    ws.onclose = () => { setStatus("disconnected"); setSessionId(null); };
    ws.onerror = () => { setStatus("disconnected"); };
  };

  useEffect(() => { connect(); }, []);

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!input.trim() || !connectionRef.current || !sessionIdRef.current) return;
    const text = input.trim();
    setInput("");
    setEvents((prev) => [...prev, { type: "user_prompt", data: { text }, timestamp: new Date().toISOString() }]);
    try {
      const resp = await connectionRef.current.prompt({
        sessionId: sessionIdRef.current,
        prompt: [{ type: "text", text }],
      });
      setEvents((prev) => [...prev, { type: "prompt_response", data: resp, timestamp: new Date().toISOString() }]);
    } catch (err: any) {
      setEvents((prev) => [...prev, { type: "error", data: { message: err.message }, timestamp: new Date().toISOString() }]);
    }
  };

  const resolvePermission = (optionId: string) => {
    if (permResolverRef.current) {
      permResolverRef.current({ outcome: { outcome: "selected", optionId } });
      permResolverRef.current = null;
      setPendingPerm(null);
    }
  };

  return (
    <>
      <header style={{ padding: "12px 16px", borderBottom: "1px solid #333", display: "flex", justifyContent: "space-between", alignItems: "center" }}>
        <h2 style={{ fontSize: 16, fontWeight: 600 }}>Spike 2 — Remote ACP Session</h2>
        <span style={{ color: status === "connected" ? "#4ade80" : status === "connecting" ? "#fbbf24" : "#ef4444", fontSize: 12 }}>
          ● {status}{sessionId ? ` (${sessionId.slice(0, 8)}…)` : ""}
        </span>
      </header>

      <div style={{ flex: 1, overflow: "auto", padding: 16 }}>
        {events.map((ev, i) => <EventRow key={i} event={ev} />)}
        <div ref={messagesEnd} />
      </div>

      {pendingPerm && (
        <div style={{ padding: 12, background: "#1a1a2e", borderTop: "1px solid #333" }}>
          <div style={{ fontSize: 13, marginBottom: 8, color: "#fbbf24" }}>Permission: {pendingPerm.toolCall?.title || "Action"}</div>
          <div style={{ display: "flex", gap: 8 }}>
            {pendingPerm.options?.map((opt: any) => (
              <button key={opt.optionId} onClick={() => resolvePermission(opt.optionId)}
                style={{ padding: "4px 12px", fontSize: 12, background: opt.kind === "allow" ? "#166534" : "#7f1d1d", color: "#e0e0e0", border: "none", borderRadius: 4, cursor: "pointer" }}>
                {opt.name}
              </button>
            ))}
          </div>
        </div>
      )}

      <form onSubmit={handleSubmit} style={{ padding: 12, borderTop: "1px solid #333", display: "flex", gap: 8 }}>
        <input value={input} onChange={(e) => setInput(e.target.value)}
          placeholder={sessionId ? "Type a prompt..." : "Connecting..."}
          disabled={!sessionId}
          style={{ flex: 1, padding: "8px 12px", background: "#111", color: "#e0e0e0", border: "1px solid #333", borderRadius: 4, fontSize: 14 }} />
        <button type="submit" disabled={!sessionId || !input.trim()}
          style={{ padding: "8px 16px", background: "#2563eb", color: "white", border: "none", borderRadius: 4, fontSize: 14, cursor: "pointer" }}>
          Send
        </button>
      </form>
    </>
  );
}

function EventRow({ event }: { event: SessionEvent }) {
  if (event.type === "user_prompt") {
    return <div style={{ padding: "4px 0", color: "#60a5fa", fontSize: 14 }}>{'> '}{event.data.text}</div>;
  }
  if (event.type === "session_update") {
    const u = event.data;
    // Try to extract text from any update shape the SDK might send
    const sessionUpdate = u.sessionUpdate || u.type;
    if (sessionUpdate === "agent_message_chunk" || sessionUpdate === "agentMessageChunk") {
      const text = u.content?.text || u.content?.content?.text || "";
      return <span style={{ color: "#a0a0a0", fontSize: 14 }}>{text}</span>;
    }
    if (sessionUpdate === "agent_thought_chunk" || sessionUpdate === "agentThoughtChunk") {
      const text = u.content?.text || "";
      return <div style={{ color: "#666", fontSize: 12, fontStyle: "italic" }}>💭 {text}</div>;
    }
    if (sessionUpdate === "tool_call" || sessionUpdate === "toolCall") {
      return <div style={{ padding: "2px 8px", margin: "2px 0", background: "#1a1a2e", borderRadius: 4, fontSize: 12, color: "#888" }}>🔧 {u.name || u.toolName || "tool"}</div>;
    }
    // Fallback: show raw update type
    if (sessionUpdate) {
      return <div style={{ fontSize: 11, color: "#555" }}>[{sessionUpdate}]</div>;
    }
    return null;
  }
  if (event.type === "prompt_response") {
    return <div style={{ padding: "4px 0", color: "#4ade80", fontSize: 12 }}>✓ {event.data.stopReason || "done"}</div>;
  }
  if (event.type === "error") {
    return <div style={{ color: "#ef4444", fontSize: 12 }}>✗ {event.data.message}</div>;
  }
  return null;
}

// ---------------------------------------------------------------------------
// State Panel
// ---------------------------------------------------------------------------

function StatePanel() {
  const [state, setState] = useState<any>(null);

  const refresh = async () => {
    try {
      const res = await fetch(STATE_URL);
      if (res.ok) setState(await res.json());
    } catch {}
  };

  useEffect(() => { refresh(); const i = setInterval(refresh, 3000); return () => clearInterval(i); }, []);

  const turns = state?.promptTurns ? Object.values(state.promptTurns) as any[] : [];
  const connections = state?.connections ? Object.values(state.connections) as any[] : [];
  const attached = connections.filter((c: any) => c.state === "attached");

  return (
    <div style={{ flex: 1, overflow: "auto", borderBottom: "1px solid #333" }}>
      <h3 style={{ padding: "8px 12px", fontSize: 13, fontWeight: 600, borderBottom: "1px solid #222", color: "#888" }}>
        Durable State <button onClick={refresh} style={{ marginLeft: 8, fontSize: 10, background: "none", border: "1px solid #444", color: "#888", borderRadius: 3, cursor: "pointer", padding: "1px 6px" }}>↻</button>
      </h3>
      <div style={{ padding: "8px 12px", fontSize: 12 }}>
        <div style={{ color: "#666" }}>Connections: {connections.length} ({attached.length} attached)</div>
        <div style={{ color: "#666" }}>Prompt turns: {turns.length}</div>
        <div style={{ color: "#666" }}>Chunks: {state?.chunks ? Object.keys(state.chunks).length : 0}</div>
      </div>
      {turns.slice(-5).map((t: any) => (
        <div key={t.promptTurnId} style={{ padding: "0 12px", fontSize: 11, color: t.state === "completed" ? "#4ade80" : t.state === "active" ? "#fbbf24" : "#888" }}>
          {t.state} — {(t.text || "").slice(0, 50)}
        </div>
      ))}
    </div>
  );
}

// ---------------------------------------------------------------------------
// File Panel
// ---------------------------------------------------------------------------

function FilePanel() {
  const [tree, setTree] = useState<any[]>([]);
  const [fileContent, setFileContent] = useState<string | null>(null);
  const [selectedFile, setSelectedFile] = useState<string | null>(null);
  const [connId, setConnId] = useState<string | null>(null);
  const [path, setPath] = useState(".");

  useEffect(() => {
    const discover = async () => {
      try {
        const res = await fetch(STATE_URL);
        const st = await res.json();
        const att = Object.values(st.connections as Record<string, any>).find((c: any) => c.state === "attached");
        if (att) setConnId(att.logicalConnectionId);
      } catch {}
    };
    discover();
    const i = setInterval(discover, 5000);
    return () => clearInterval(i);
  }, []);

  useEffect(() => {
    if (!connId) return;
    fetch(`/api/v1/connections/${connId}/fs/tree?path=${encodeURIComponent(path)}`)
      .then((r) => r.json()).then(setTree).catch(() => setTree([]));
  }, [connId, path]);

  const openFile = async (name: string) => {
    if (!connId) return;
    const fp = path === "." ? name : `${path}/${name}`;
    try {
      const res = await fetch(`/api/v1/connections/${connId}/files?path=${encodeURIComponent(fp)}`);
      setFileContent(res.ok ? await res.text() : `Error: ${res.status}`);
      setSelectedFile(fp);
    } catch (e: any) { setFileContent(`Error: ${e.message}`); }
  };

  return (
    <div style={{ flex: 1, overflow: "auto" }}>
      <h3 style={{ padding: "8px 12px", fontSize: 13, fontWeight: 600, borderBottom: "1px solid #222", color: "#888" }}>
        Files {connId ? "" : "(waiting…)"}
      </h3>
      {path !== "." && (
        <div style={{ padding: "4px 12px" }}>
          <button onClick={() => { setPath(path.split("/").slice(0, -1).join("/") || "."); setFileContent(null); }}
            style={{ fontSize: 11, background: "none", border: "none", color: "#60a5fa", cursor: "pointer" }}>← back</button>
        </div>
      )}
      <div style={{ padding: "0 12px" }}>
        {tree.map((e: any) => (
          <div key={e.name} style={{ fontSize: 12, padding: "2px 0", cursor: "pointer" }}
            onClick={() => e.type === "directory" ? setPath(path === "." ? e.name : `${path}/${e.name}`) : openFile(e.name)}>
            <span style={{ color: e.type === "directory" ? "#60a5fa" : "#e0e0e0" }}>
              {e.type === "directory" ? "📁 " : "📄 "}{e.name}
            </span>
          </div>
        ))}
      </div>
      {fileContent && (
        <div style={{ margin: "8px 12px", padding: 8, background: "#111", borderRadius: 4, maxHeight: 200, overflow: "auto" }}>
          <div style={{ fontSize: 11, color: "#666", marginBottom: 4 }}>{selectedFile}</div>
          <pre style={{ fontSize: 11, color: "#a0a0a0", whiteSpace: "pre-wrap" }}>{fileContent.slice(0, 2000)}</pre>
        </div>
      )}
    </div>
  );
}
