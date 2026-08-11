import { execFileSync } from "node:child_process";
import { existsSync, readdirSync, readFileSync, rmSync } from "node:fs";
import path from "node:path";
import { dataDir } from "../../wdio.conf.js";
import { archiveTask, openTask, requireTermicApi, snap, waitForAppShell } from "../helpers";

// Plan capture: claude's plan-mode plan lands in the task's .context/plans/
// so it is readable in Termic instead of only in the global ~/.claude/plans.
//
// The hook body (`termic capture-plan`) is what these cases exercise, driven
// with the SAME payloads a real claude v2.1.226 emits. Driving actual plan
// mode is not an option here: it needs a live model, a human at the approval
// prompt, and it is nondeterministic. What the unit tests cannot see, and
// this file can, is the part that spans processes: the app scaffolds
// `.context/plans/`, the shipped binary writes into it, and the app's own
// directory listing then surfaces the file to the file tree.
//
// Payload shapes (verified against the real CLI):
//   PreToolUse   tool_input:    { plan, planFilePath }   <- before approval
//   PostToolUse  tool_input:    {}   (empty)
//                tool_response: { plan, filePath, isAgent }

const PLAN_FILE = "/Users/e2e/.claude/plans/e2e-fixture.md";
const SESSION = "e2e-session-0001";

const prePayload = (plan: string) => JSON.stringify({
  session_id: SESSION,
  hook_event_name: "PreToolUse",
  tool_name: "ExitPlanMode",
  tool_input: { plan, planFilePath: PLAN_FILE },
});

const postPayload = (plan: string) => JSON.stringify({
  session_id: SESSION,
  hook_event_name: "PostToolUse",
  tool_name: "ExitPlanMode",
  tool_input: {},
  tool_response: { plan, filePath: PLAN_FILE, isAgent: false },
});

/** The CLI as shipped, built alongside the e2e binary. Anchored to the repo
 *  root the way wdio.conf does, not to cwd. */
const cliPath = path.join(path.dirname(dataDir), "..", "src-tauri", "target", "debug", "termic-cli");

describe("plan capture into .context/plans", () => {
  let taskId: string | undefined;
  let taskPath = "";
  let plansDir = "";

  before(async () => {
    await waitForAppShell();
    await requireTermicApi();
    taskId = await openTask("e2e-plan-capture");
    taskPath = await browser.execute(
      (id) => window.__termic!.useApp.getState().tasks.find((t: any) => t.id === id)!.path as string,
      taskId,
    );
    plansDir = path.join(taskPath, ".context", "plans");
  });

  after(async () => {
    // The capture dir is scratch, and a stale file would let a later run pass
    // on the previous run's artifact.
    rmSync(path.join(taskPath, ".context"), { recursive: true, force: true });
    if (taskId) await archiveTask(taskId);
  });

  /** Run the hook body exactly as claude would: payload on stdin, context dir
   *  in the env. Returns nothing on purpose; the verb is silent by contract. */
  function runHook(payload: string): string {
    return execFileSync(cliPath, ["capture-plan"], {
      input: payload,
      env: { ...process.env, TERMIC_CONTEXT_DIR: path.join(taskPath, ".context"), TERMIC_TASK_ID: taskId },
      encoding: "utf8",
    });
  }

  const captures = () => (existsSync(plansDir) ? readdirSync(plansDir).filter(f => f.endsWith(".md")) : []);

  it("scaffolds .context/plans in a new task worktree", () => {
    // ensure_context_dirs runs at task creation, so the hook always has a
    // destination even before the first agent spawn.
    expect(existsSync(plansDir)).toBe(true);
  });

  it("writes the plan before the user approves it", async () => {
    // PreToolUse is the case that matters: it fires while the approval prompt
    // is still up, which is the whole point of the feature.
    const stdout = runHook(prePayload("# E2E plan\n\nStep one.\nStep two."));
    // Silence is a contract: a PreToolUse hook's stdout can carry permission
    // decisions, so anything printed here could alter the agent's turn.
    expect(stdout).toBe("");

    const files = captures();
    expect(files).toHaveLength(1);
    expect(files[0]).toMatch(/^\d{8}T\d{6}Z_e2e-plan\.md$/);

    const body = readFileSync(path.join(plansDir, files[0]), "utf8");
    expect(body).toContain("Step one.");
    expect(body).toContain(`<!-- termic-session: ${SESSION} -->`);
    await snap("plan-capture-written");
  });

  it("shows the captured plan in the app's file tree", async () => {
    // The app's own directory listing, the one the All Files tree renders
    // from. `.context` is dot-prefixed but not excluded, so it must show up.
    const dotContext = await browser.execute(
      async (id) => (await window.__termic!.ipc.taskDirList(id, "")).map((e: any) => e.name),
      taskId,
    );
    expect(dotContext).toContain(".context");

    const listed = await browser.execute(
      async (id) => (await window.__termic!.ipc.taskDirList(id, ".context/plans")).map((e: any) => e.name),
      taskId,
    );
    expect(listed).toEqual(captures());
  });

  it("treats the approval that follows as a no-op", () => {
    // PostToolUse carries the same plan, so the file must not be rewritten:
    // an mtime bump would set the file tree refreshing for nothing.
    const before = captures();
    runHook(postPayload("# E2E plan\n\nStep one.\nStep two."));
    expect(captures()).toEqual(before);
    expect(readFileSync(path.join(plansDir, before[0]), "utf8")).toContain("Step one.");
  });

  it("updates the same file when the plan is revised", () => {
    // The reject-comment-revise loop: same session and same source plan file,
    // so the revision must land in the original capture rather than fork a
    // second one, even though its title changed.
    const [original] = captures();
    runHook(prePayload("# E2E plan, take two\n\nStep one.\nA better step two."));

    expect(captures()).toEqual([original]);
    const body = readFileSync(path.join(plansDir, original), "utf8");
    expect(body).toContain("A better step two.");
    expect(body).toContain("take two");
  });

  it("gives an unrelated plan its own file", () => {
    // A different session AND a different plan file is a different artifact.
    const before = captures();
    runHook(JSON.stringify({
      session_id: "e2e-session-0002",
      hook_event_name: "PreToolUse",
      tool_name: "ExitPlanMode",
      tool_input: { plan: "# Another plan\n\nUnrelated.", planFilePath: "/Users/e2e/.claude/plans/other.md" },
    }));

    const after = captures();
    expect(after).toHaveLength(before.length + 1);
    expect(after.some(f => f.endsWith("_another-plan.md"))).toBe(true);
  });

  it("ignores payloads with no plan and never fails the agent's turn", () => {
    const before = captures();
    for (const payload of ["", "not json at all", "{}", '{"tool_input":{"plan":"   "}}']) {
      // execFileSync throws on a non-zero exit, so this asserts exit 0 too:
      // a hook that errors surfaces as a failure in the agent's transcript.
      expect(runHook(payload)).toBe("");
    }
    expect(captures()).toEqual(before);
  });
});
