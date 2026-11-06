import { useState } from "react";
import {
  Terminal as TerminalIcon,
  MessageSquare,
  Smartphone,
  GitBranch,
  Plus,
  Settings,
  CornerDownLeft,
  Shield,
} from "lucide-react";
import type { AgentKind, WorkspaceInfo } from "./types/protocol";

export default function App() {
  const [activeLens, setActiveLens] = useState<"chat" | "terminal">("chat");
  const [composerText, setComposerText] = useState("");
  const [activeAgent, setActiveAgent] = useState<AgentKind>("claude");

  // Mock initial state for preview and demonstration
  const workspaces: WorkspaceInfo[] = [
    {
      id: "ws-1",
      name: "checkout-api",
      working_dir: "C:/projects/checkout-api",
      tabs: [],
      active_tab_id: null,
    },
    {
      id: "ws-2",
      name: "web-dashboard",
      working_dir: "C:/projects/web-dashboard",
      tabs: [],
      active_tab_id: null,
    },
  ];

  return (
    <div className="flex h-screen w-screen overflow-hidden bg-[#120f18] text-[#ede8f5] font-sans">
      {/* Sidebar: Workspaces & Live Agent Status */}
      <aside className="w-64 border-r border-[#2a2436] bg-[#191522] flex flex-col justify-between select-none">
        <div>
          {/* Logo / Brand Header */}
          <div className="flex items-center justify-between p-4 border-b border-[#2a2436]">
            <div className="flex items-center gap-2">
              <div className="h-6 w-6 rounded bg-[#e07a93] flex items-center justify-center font-bold text-[#120f18] text-xs">
                O
              </div>
              <span className="font-semibold tracking-wide text-sm">Orion</span>
            </div>
            <span className="text-[10px] px-1.5 py-0.5 rounded bg-[#2a2436] text-[#9f94b0] font-mono">
              v0.1.0
            </span>
          </div>

          {/* Workspaces List */}
          <div className="p-3">
            <div className="flex items-center justify-between mb-2 px-1">
              <span className="text-xs uppercase tracking-wider text-[#6b6378] font-semibold">
                Workspaces
              </span>
              <button
                className="text-[#9f94b0] hover:text-[#e07a93] p-1 rounded"
                title="New Workspace"
              >
                <Plus size={14} />
              </button>
            </div>

            <div className="space-y-1">
              {workspaces.map((ws, i) => (
                <div
                  key={ws.id}
                  className={`flex items-center justify-between px-2.5 py-2 rounded-md text-xs cursor-pointer transition-colors ${
                    i === 0
                      ? "bg-[#221c2e] text-[#ede8f5] border-l-2 border-[#e07a93]"
                      : "text-[#9f94b0] hover:bg-[#221c2e]/60"
                  }`}
                >
                  <div className="flex items-center gap-2 truncate">
                    <span className="h-2 w-2 rounded-full bg-[#38bdf8] animate-pulse" />
                    <span className="truncate font-medium">{ws.name}</span>
                  </div>
                  <span className="text-[10px] font-mono text-[#6b6378]">
                    Claude
                  </span>
                </div>
              ))}
            </div>
          </div>
        </div>

        {/* Tailscale / Mobile Status Badge */}
        <div className="p-3 border-t border-[#2a2436] bg-[#16121f]">
          <div className="flex items-center justify-between text-xs text-[#9f94b0] mb-2">
            <div className="flex items-center gap-1.5">
              <Smartphone size={14} className="text-[#34d399]" />
              <span>Tailscale Active</span>
            </div>
            <button
              className="text-[10px] text-[#e07a93] hover:underline"
              title="Pair Phone"
            >
              Pair
            </button>
          </div>
          <div className="text-[10px] font-mono text-[#6b6378] truncate">
            https://workstation.tailnet.ts.net:7317
          </div>
        </div>
      </aside>

      {/* Main Content Area */}
      <main className="flex-1 flex flex-col h-full bg-[#120f18] overflow-hidden">
        {/* Top App Bar & Dual-Lens Switcher */}
        <header className="h-12 border-b border-[#2a2436] bg-[#191522] px-4 flex items-center justify-between select-none">
          <div className="flex items-center gap-3">
            <span className="font-medium text-sm">checkout-api</span>
            <div className="flex items-center gap-1 text-xs text-[#6b6378] font-mono">
              <GitBranch size={12} />
              <span>main</span>
            </div>
            <span className="text-[11px] px-2 py-0.5 rounded bg-[#2a2436] text-[#e07a93] font-mono">
              Agent: {activeAgent}
            </span>
          </div>

          {/* Dual-Lens Pill Switcher */}
          <div className="flex items-center bg-[#120f18] p-0.5 rounded-lg border border-[#2a2436]">
            <button
              onClick={() => setActiveLens("chat")}
              className={`flex items-center gap-1.5 px-3 py-1 rounded-md text-xs font-medium transition-colors ${
                activeLens === "chat"
                  ? "bg-[#221c2e] text-[#ede8f5] shadow-sm"
                  : "text-[#9f94b0] hover:text-[#ede8f5]"
              }`}
            >
              <MessageSquare size={13} />
              <span>Chat</span>
            </button>
            <button
              onClick={() => setActiveLens("terminal")}
              className={`flex items-center gap-1.5 px-3 py-1 rounded-md text-xs font-medium transition-colors ${
                activeLens === "terminal"
                  ? "bg-[#221c2e] text-[#ede8f5] shadow-sm"
                  : "text-[#9f94b0] hover:text-[#ede8f5]"
              }`}
            >
              <TerminalIcon size={13} />
              <span>Terminal</span>
            </button>
          </div>

          <div className="flex items-center gap-2">
            <button
              onClick={() => setActiveAgent(activeAgent === "claude" ? "codex" : "claude")}
              className="px-2 py-1 text-xs text-[#9f94b0] hover:text-[#ede8f5] bg-[#221c2e] border border-[#2a2436] rounded"
            >
              Switch Agent
            </button>
            <button
              className="p-1.5 text-[#9f94b0] hover:text-[#ede8f5] hover:bg-[#221c2e] rounded"
              title="Settings"
            >
              <Settings size={16} />
            </button>
          </div>
        </header>

        {/* Viewport: Chat or Live Terminal */}
        <div className="flex-1 overflow-y-auto p-4 flex flex-col justify-between">
          {activeLens === "chat" ? (
            <div className="space-y-4 max-w-3xl mx-auto w-full">
              {/* User Prompt Turn */}
              <div className="flex flex-col items-end">
                <div className="bg-[#221c2e] border border-[#2a2436] rounded-xl px-4 py-3 max-w-[85%] text-sm">
                  Add rate limiting to the Stripe checkout webhook endpoint and
                  create a test for 429 responses.
                </div>
                <span className="text-[10px] text-[#6b6378] mt-1 pr-1 font-mono">
                  12:42 PM
                </span>
              </div>

              {/* Agent Response Turn with Tool Work Block */}
              <div className="flex flex-col items-start space-y-2">
                <div className="flex items-center gap-2">
                  <span className="h-2 w-2 rounded-full bg-[#38bdf8]" />
                  <span className="text-xs font-medium text-[#ede8f5]">
                    Claude Code
                  </span>
                  <span className="text-[10px] font-mono text-[#6b6378]">
                    Worked for 32s
                  </span>
                </div>

                <div className="bg-[#191522] border border-[#2a2436] rounded-xl p-4 w-full space-y-3 text-sm">
                  <p className="text-[#ede8f5]">
                    I analyzed `src/routes/webhook.ts` and added token-bucket
                    rate limiting. Here is the implementation:
                  </p>

                  <div className="bg-[#120f18] rounded-lg border border-[#2a2436] p-3 font-mono text-xs text-[#9f94b0]">
                    <div className="flex items-center justify-between pb-2 mb-2 border-b border-[#2a2436] text-[11px] text-[#6b6378]">
                      <span>src/routes/webhook.ts</span>
                      <span className="text-[#34d399]">+24 / -3 lines</span>
                    </div>
                    <code>
                      + const limiter = new TokenBucketRateLimiter(60, 100);
                      <br />+ app.post("/api/webhook", limiter.middleware(),
                      async (req, res) =&gt; &#123;
                    </code>
                  </div>

                  {/* Interactive Approval Card */}
                  <div className="border border-[#e07a93]/40 bg-[#e07a93]/10 rounded-lg p-3 space-y-2">
                    <div className="flex items-center gap-2 text-xs font-semibold text-[#e07a93]">
                      <Shield size={14} />
                      <span>Permission Request: Execute Command</span>
                    </div>
                    <p className="text-xs text-[#ede8f5]">
                      Agent requested permission to run:
                      <code className="block mt-1 font-mono bg-[#120f18] p-1.5 rounded border border-[#2a2436]">
                        bun test tests/webhook.test.ts
                      </code>
                    </p>
                    <div className="flex items-center gap-2 pt-1">
                      <button className="px-3 py-1 bg-[#e07a93] text-[#120f18] rounded text-xs font-semibold hover:bg-[#e88fa5] transition-colors">
                        Allow Once
                      </button>
                      <button className="px-3 py-1 bg-[#221c2e] text-[#ede8f5] border border-[#2a2436] rounded text-xs hover:bg-[#2a2436] transition-colors">
                        Deny
                      </button>
                      <button className="px-3 py-1 text-xs text-[#9f94b0] hover:text-[#ede8f5]">
                        Always in Session
                      </button>
                    </div>
                  </div>
                </div>
              </div>
            </div>
          ) : (
            /* Live Terminal View */
            <div className="h-full flex flex-col justify-between bg-[#120f18] rounded-lg border border-[#2a2436] p-3 font-mono text-xs">
              <div className="text-[#34d399]">
                [orion-pty] Attached to Claude Code session (PID: 14820)
                <br />
                <span className="text-[#9f94b0]">
                  $ bun test tests/webhook.test.ts
                  <br />
                  &nbsp;✓ webhook rate limit rejects excess calls with 429 (12ms)
                  <br />
                  &nbsp;✓ valid signature processes normally (24ms)
                  <br />
                  <br />
                  5 pass, 0 fail
                </span>
              </div>

              {/* Mobile Touch Key Bar */}
              <div className="h-10 border-t border-[#2a2436] pt-2 flex items-center justify-between text-xs text-[#9f94b0] select-none">
                <div className="flex items-center gap-1.5 overflow-x-auto">
                  {["Esc", "Tab", "Ctrl", "Alt", "Shift", "↑", "↓", "←", "→"].map(
                    (key) => (
                      <button
                        key={key}
                        className="px-2.5 py-1 bg-[#221c2e] border border-[#2a2436] rounded text-[11px] font-mono hover:bg-[#2a2436] active:bg-[#e07a93] active:text-[#120f18]"
                      >
                        {key}
                      </button>
                    )
                  )}
                </div>
                <button className="px-3 py-1 bg-[#221c2e] border border-[#2a2436] rounded text-[11px] font-mono text-[#e07a93]">
                  Ctrl+C
                </button>
              </div>
            </div>
          )}

          {/* Bottom Chat Composer */}
          {activeLens === "chat" && (
            <div className="max-w-3xl mx-auto w-full pt-4">
              <div className="relative border border-[#2a2436] bg-[#191522] rounded-xl p-2 focus-within:border-[#e07a93] transition-colors">
                <textarea
                  value={composerText}
                  onChange={(e) => setComposerText(e.target.value)}
                  placeholder="Send instructions, @file mentions, or /command..."
                  className="w-full bg-transparent resize-none text-sm outline-none px-2 py-1 min-h-[50px] text-[#ede8f5] placeholder:text-[#6b6378]"
                  rows={2}
                />
                <div className="flex items-center justify-between px-2 pt-1 border-t border-[#2a2436]/60">
                  <div className="flex items-center gap-2 text-xs text-[#6b6378]">
                    <span>@ files</span>
                    <span>/ commands</span>
                  </div>
                  <button
                    disabled={!composerText.trim()}
                    className="flex items-center gap-1.5 px-3 py-1 rounded bg-[#e07a93] text-[#120f18] font-medium text-xs disabled:opacity-30 disabled:cursor-not-allowed hover:bg-[#e88fa5] transition-all"
                  >
                    <span>Send</span>
                    <CornerDownLeft size={12} />
                  </button>
                </div>
              </div>
            </div>
          )}
        </div>
      </main>
    </div>
  );
}
