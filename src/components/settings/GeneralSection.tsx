// General settings: the app-level things that are not about tasks, agents,
// notifications, or the sandbox. Everything else that used to pile up here
// moved to its own rail item (Tasks / Notifications / Sandbox / CLI); this
// page is deliberately short.
//
// Loads the full Settings object so that saves preserve other fields
// (agents, etc.) instead of wiping them.

import { useEffect, useRef, useState } from "react";
import { settingsSave } from "@/lib/ipc";
import type { Settings } from "@/lib/types";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { Button } from "@/components/ui/Button";
import { Input } from "@/components/ui/Input";
import { usePrefs } from "@/store/prefs";
import { useUI } from "@/store/ui";
import { useApp } from "@/store/app";
import { ExcludeEditor } from "./ExcludeEditor";
import { Block, SectionTitle, Toggle, useBackendSettings } from "./Controls";
import { cn, cleanLines } from "@/lib/utils";
import { IS_MAC } from "@/lib/shortcuts";

export function GeneralSection() {
  const { settings, store, patch } = useBackendSettings();
  // What the window's close button does. A backend Settings field Rust
  // re-reads on every close, so a change here applies without a restart.
  // Three-way rather than a toggle because "ask me" has to remain reachable:
  // ticking "Don't ask again" in the close prompt is otherwise a one-way door.
  const [closeAction, setCloseAction] = useState<"ask" | "menubar" | "quit">("ask");
  // Whether the menu-bar item (Show/Quit Termic, the attention dropdown) is
  // shown at all. Also a backend field Rust re-reads live, on every save.
  const [trayEnabled, setTrayEnabled] = useState(true);
  // Whether claude's plan-mode plan is mirrored into the task's
  // .context/plans/. Backend field, read at spawn time, so a change applies
  // to the next agent start rather than to running ones.
  const [capturePlans, setCapturePlans] = useState(true);
  const [reposDir, setReposDir] = useState("");
  const [originalDir, setOriginalDir] = useState("");
  const [busy, setBusy] = useState(false);
  // Personal (global) file-tree exclude globs. Kept as an array so the
  // ExcludeEditor's preset chips can add/remove cleanly; joined for the
  // dirty check.
  const [fileExclude, setFileExclude] = useState<string[]>([]);
  const [fileExcludeOriginal, setFileExcludeOriginal] = useState("");

  useEffect(() => {
    if (!settings) return;
    setCloseAction(settings.close_action ?? "ask");
    setTrayEnabled(settings.tray_enabled ?? true);
    setCapturePlans(settings.capture_plans ?? true);
  }, [settings]);

  async function saveCloseAction(v: "ask" | "menubar" | "quit") {
    if (!settings) return;
    const prev = closeAction;
    setCloseAction(v);
    if (!(await patch({ close_action: v }))) {
      setCloseAction(prev);   // persist failed: don't show unsaved state
    }
  }

  async function saveTrayEnabled(v: boolean) {
    if (!settings) return;
    const prev = trayEnabled;
    setTrayEnabled(v);
    if (!(await patch({ tray_enabled: v }))) {
      setTrayEnabled(prev);
    }
  }

  async function saveCapturePlans(v: boolean) {
    if (!settings) return;
    const prev = capturePlans;
    setCapturePlans(v);
    if (!(await patch({ capture_plans: v }))) {
      setCapturePlans(prev);
    }
  }

  const loadRemoteImages = usePrefs(s => s.loadRemoteImages);
  const setLoadRemoteImages = usePrefs(s => s.setLoadRemoteImages);

  // Hydrate the local edit buffers once, when the backend Settings land. The
  // ref gate matters: a later save re-publishes `settings`, and re-running
  // this would stomp whatever the user has typed since.
  const hydrated = useRef(false);
  useEffect(() => {
    if (!settings || hydrated.current) return;
    hydrated.current = true;
    setReposDir(settings.repos_dir);
    setOriginalDir(settings.repos_dir);
    const ex = settings.file_tree_exclude ?? [];
    setFileExclude(ex);
    setFileExcludeOriginal(ex.join("\n"));
  }, [settings]);

  // Scroll-to-and-flash a specific row (e.g. the remote-images banner's
  // "Settings" link) once, on mount — see view.settingsHighlight. Consumed
  // immediately so a later manual visit to General doesn't re-trigger it,
  // and cleared after a beat regardless (a fresh Settings mount from a
  // stale link a minute later shouldn't re-flash something the user is
  // already looking at).
  const settingsHighlight = useApp(s => s.view.settingsHighlight);
  const [flashId, setFlashId] = useState<string | null>(null);
  useEffect(() => {
    if (!settingsHighlight) return;
    const id = settingsHighlight;
    useApp.getState().clearSettingsHighlight();
    document.getElementById(`setting-${id}`)?.scrollIntoView({ behavior: "smooth", block: "center" });
    setFlashId(id);
    const t = window.setTimeout(() => setFlashId(f => (f === id ? null : f)), 1600);
    return () => window.clearTimeout(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [settingsHighlight]);

  const excludeDirty = fileExclude.join("\n") !== fileExcludeOriginal;
  const dirty = reposDir !== originalDir;

  async function browse() {
    const sel = await openDialog({ directory: true, multiple: false });
    if (typeof sel === "string") setReposDir(sel);
  }
  async function save() {
    if (!settings) return;
    setBusy(true);
    try {
      const next: Settings = { ...settings, repos_dir: reposDir.trim(), welcomed: true };
      await settingsSave(next);
      store(next);
      setOriginalDir(reposDir.trim());
    } finally { setBusy(false); }
  }
  async function saveExclude() {
    if (!settings) return;
    setBusy(true);
    try {
      const cleaned = cleanLines(fileExclude);
      const next: Settings = { ...settings, file_tree_exclude: cleaned };
      await settingsSave(next);
      store(next);
      setFileExclude(cleaned);
      setFileExcludeOriginal(cleaned.join("\n"));
      // The file tree is hidden behind this Settings overlay; force it to
      // re-read so the new excludes apply the moment the user looks back.
      useUI.getState().reloadFileTree();
    } finally { setBusy(false); }
  }

  return (
    <div className="flex flex-col gap-7">
      <SectionTitle title="General" />

      <Block first>
        <div className="text-[14px] font-medium">Repos directory</div>
        <div className="mt-0.5 text-[12.5px] text-[var(--color-fg-dim)]">
          Where Termic scans for unadded git repos when you click "Add project".
        </div>
        <div className="mt-2 flex gap-2">
          <Input value={reposDir} onChange={(e) => setReposDir(e.target.value)} placeholder="~/Projects" className="font-mono" />
          <Button variant="secondary" onClick={browse}>Browse…</Button>
        </div>
        <div className="mt-3">
          <Button variant="primary" disabled={!dirty || busy} onClick={save}>
            {busy ? "Saving…" : "Save"}
          </Button>
        </div>
      </Block>

      {/* Personal file-tree excludes. Hide noise (caches, venvs, build
          output) from the "All files" tree across every project on this
          machine. Per-project, team-shared excludes live in each repo's
          .termic.yaml (Settings → Projects). */}
      <Block>
        <div className="text-[14px] font-medium">Hidden files (personal)</div>
        <div className="mt-0.5 text-[12.5px] text-[var(--color-fg-dim)]">
          Patterns hidden from the "All files" tree across every project on this machine. Pick a preset or add your own. For team-shared, per-repo excludes, use a project's <code className="font-mono">.termic.yaml</code> (Settings → Projects).
        </div>
        <div className="mt-3">
          <ExcludeEditor value={fileExclude} onChange={setFileExclude} />
        </div>
        <div className="mt-3">
          <Button variant="primary" disabled={!excludeDirty || busy} onClick={saveExclude}>
            {busy ? "Saving…" : "Save hidden files"}
          </Button>
        </div>
      </Block>

      {/* macOS only: the CloseRequested handler that reads close_action is
          #[cfg(target_os = "macos")], because Windows and most Linux desktops
          expect close to quit (docs/plans/windows.md). Rendering the control
          elsewhere would save a setting nothing reads. */}
      {IS_MAC && <Block id="setting-close-action">
        <div className="flex flex-col gap-1">
          <div className="text-[13.5px] text-[var(--color-fg)]">When you close the window</div>
          <p className="text-[12.5px] text-[var(--color-fg-dim)] leading-relaxed max-w-2xl">
            Closing used to quit Termic and stop every running agent. Keeping
            them in the menu bar leaves them working, and Quit (⌘Q, or the
            menu-bar item) becomes the only thing that stops them.
          </p>
          <div className="mt-2 max-w-sm">
            <select
              value={closeAction}
              onChange={(e) => saveCloseAction(e.target.value as "ask" | "menubar" | "quit")}
              className="h-9 w-full rounded-lg border border-[var(--color-border)] bg-[var(--color-bg)] pl-3 pr-8 text-[13px] text-[var(--color-fg)] outline-none transition-colors focus:border-[var(--color-accent)] focus:ring-[3px] focus:ring-[var(--color-accent-soft)]"
              data-testid="close-action-select"
            >
              <option value="ask">Ask me each time</option>
              <option value="menubar">Keep agents running in the menu bar</option>
              <option value="quit">Quit Termic and stop agents</option>
            </select>
          </div>
        </div>
      </Block>}

      <Block id="setting-tray-enabled">
        <Toggle
          label="Show Termic in the menu bar"
          hint={
            "A small icon that's always there while Termic is running: a badge and dropdown for tasks that need your input or just finished, and Show/Quit."
            + (IS_MAC
              ? " Turning it off also means closing to the menu bar (above) falls back to the dock icon as your way back in."
              : "")
          }
          value={trayEnabled}
          onChange={saveTrayEnabled}
        />
      </Block>

      <Block id="setting-capture-plans">
        <Toggle
          label="Save agent plans to the task's .context folder"
          hint="Claude writes plan-mode plans to a global folder outside your project, where Termic can't show them. With this on, the plan is copied into the task's .context/plans as it's presented, so you can read it in the file tree while you decide whether to approve it. Revisions update the same file. Applies to the next agent you start."
          value={capturePlans}
          onChange={saveCapturePlans}
        />
      </Block>

      <Block
        id="setting-load-remote-images"
        className={cn(
          "rounded-md transition-colors duration-700",
          flashId === "load-remote-images" && "bg-[var(--color-accent-deep)]/15",
        )}
      >
        <Toggle
          label="Load remote images in markdown preview"
          hint="Off by default: images hosted on external sites are blocked in the markdown preview, so opening an untrusted file (a dependency's README, a fetched page) can't silently fire a network request. A per-document button in the preview can still load them for just that file."
          value={loadRemoteImages}
          onChange={setLoadRemoteImages}
        />
      </Block>
    </div>
  );
}
