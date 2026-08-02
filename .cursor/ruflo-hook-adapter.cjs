#!/usr/bin/env node
/**
 * ruflo <-> Cursor hook adapter.
 *
 * Cursor Agent Hooks speak a different protocol than Claude Code:
 *   - Cursor pipes an event-specific JSON object on stdin and expects a JSON
 *     object back on stdout (e.g. {"permission":"allow"} for shell hooks).
 *   - ruflo's .claude/helpers/hook-handler.cjs expects Claude Code's shape
 *     (tool_input/tool_name/prompt) and prints HUMAN-READABLE text to stdout.
 *
 * This adapter sits between them: it reads Cursor's payload, invokes the
 * ruflo hook-handler with the matching subcommand + a synthesized
 * Claude-shaped stdin, captures (and hides) ruflo's text output, and emits a
 * valid Cursor JSON response. ruflo runs purely for its side effects
 * (routing, learning, session memory, safety check).
 *
 * Event map (Cursor -> ruflo subcommand):
 *   beforeShellExecution -> pre-bash   (deny on non-zero exit, else allow)
 *   afterFileEdit        -> post-edit  (records edit for learning)
 *   beforeSubmitPrompt   -> route      (+ one-time session bootstrap)
 *   stop                 -> session-end (+ auto-memory sync)
 *
 * Fail-open by design: any adapter error defaults to allow/continue so a
 * ruflo hiccup never blocks the user's action in Cursor.
 *
 * Usage (from .cursor/hooks.json):
 *   node .cursor/ruflo-hook-adapter.cjs <eventName>
 */

const path = require('path');
const fs = require('fs');
const os = require('os');
const { spawnSync, spawn } = require('child_process');

// The ruflo helpers live one level up from this .cursor/ directory.
const ROOT = path.resolve(__dirname, '..');
const HELPERS = path.join(ROOT, '.claude', 'helpers');
const HOOK_HANDLER = path.join(HELPERS, 'hook-handler.cjs');
const AUTO_MEMORY = path.join(HELPERS, 'auto-memory-hook.mjs');
const SESSION_MARKER_DIR = path.join(os.tmpdir(), 'ruflo-cursor-sessions');

// ----- stdin (Cursor payload) ---------------------------------------------

function readStdinSync() {
  try {
    // fd 0; blocks until EOF. Cursor closes the pipe after writing.
    return fs.readFileSync(0, 'utf8');
  } catch (e) {
    return '';
  }
}

function emit(obj) {
  process.stdout.write(JSON.stringify(obj));
  process.exit(0);
}

// ----- ruflo hook-handler invocation --------------------------------------

// Run hook-handler synchronously with a synthesized Claude-shaped payload.
// Returns the child's exit status (0 on success). ruflo's own stdout/stderr
// are captured and forwarded to OUR stderr only (never Cursor's stdout).
function runHandler(subcommand, payload, extraEnv) {
  if (!fs.existsSync(HOOK_HANDLER)) return 0; // ruflo not present -> no-op
  const res = spawnSync('node', [HOOK_HANDLER, subcommand], {
    input: JSON.stringify(payload || {}),
    env: Object.assign({}, process.env, extraEnv || {}),
    encoding: 'utf8',
    timeout: 5000,
  });
  if (res.stdout) process.stderr.write(`[ruflo:${subcommand}] ${res.stdout}`);
  if (res.stderr) process.stderr.write(`[ruflo:${subcommand}:err] ${res.stderr}`);
  // spawnSync sets status=null on timeout/signal; treat that as "did not block".
  return typeof res.status === 'number' ? res.status : 0;
}

// Fire-and-forget: for session bootstrap / memory work that must not add
// latency to the user's prompt. Detached + unref so it outlives this adapter.
function runDetached(cmd, cmdArgs) {
  try {
    const child = spawn(cmd, cmdArgs, {
      detached: true,
      stdio: 'ignore',
      env: process.env,
    });
    child.unref();
  } catch (e) { /* best-effort */ }
}

// Run ruflo's session-restore + auto-memory import exactly once per Cursor
// conversation (Cursor has no SessionStart event). Keyed on conversation_id
// via a marker file; detached so the first prompt isn't slowed down.
function bootstrapSessionOnce(conversationId) {
  if (!conversationId) return;
  try {
    fs.mkdirSync(SESSION_MARKER_DIR, { recursive: true });
    const safe = String(conversationId).replace(/[^A-Za-z0-9_-]/g, '_').slice(0, 128);
    const marker = path.join(SESSION_MARKER_DIR, safe);
    if (fs.existsSync(marker)) return; // already bootstrapped
    fs.writeFileSync(marker, new Date().toISOString());
  } catch (e) {
    return; // if we can't track it, skip rather than re-run every prompt
  }
  if (fs.existsSync(HOOK_HANDLER)) runDetached('node', [HOOK_HANDLER, 'session-restore']);
  if (fs.existsSync(AUTO_MEMORY)) runDetached('node', [AUTO_MEMORY, 'import']);
}

// ----- event handlers ------------------------------------------------------

function pick(obj, keys) {
  for (const k of keys) {
    if (obj && obj[k] != null && obj[k] !== '') return obj[k];
  }
  return '';
}

function handleBeforeShellExecution(input) {
  const command = String(
    pick(input, ['command']) || pick(input.tool_input || {}, ['command']) || ''
  );
  const status = runHandler(
    'pre-bash',
    { tool_name: 'Bash', tool_input: { command } },
    { TOOL_INPUT_command: command }
  );
  if (status !== 0) {
    emit({
      permission: 'deny',
      userMessage: 'Blocked by ruflo safety check.',
      agentMessage: 'ruflo pre-bash flagged this command as dangerous; it was not run.',
    });
  }
  emit({ permission: 'allow' });
}

function handleAfterFileEdit(input) {
  const filePath = String(
    pick(input, ['file_path', 'filePath', 'path']) ||
    pick(input.tool_input || {}, ['file_path']) || ''
  );
  runHandler(
    'post-edit',
    { tool_name: 'Edit', tool_input: { file_path: filePath } },
    { TOOL_INPUT_file_path: filePath }
  );
  emit({});
}

function handleBeforeSubmitPrompt(input) {
  const prompt = String(pick(input, ['prompt', 'text', 'message']) || '');
  bootstrapSessionOnce(pick(input, ['conversation_id', 'conversationId']));
  runHandler('route', { prompt }, { PROMPT: prompt });
  emit({ continue: true });
}

function handleStop(input) {
  runHandler('session-end', {});
  if (fs.existsSync(AUTO_MEMORY)) runDetached('node', [AUTO_MEMORY, 'sync']);
  emit({});
}

// ----- dispatch ------------------------------------------------------------

function main() {
  const raw = readStdinSync();
  let input = {};
  if (raw && raw.trim()) {
    try { input = JSON.parse(raw); } catch (e) { input = {}; }
  }
  // Prefer the event from the payload; fall back to argv for robustness.
  const event = pick(input, ['hook_event_name', 'hookEventName']) || process.argv[2] || '';

  switch (event) {
    case 'beforeShellExecution': return handleBeforeShellExecution(input);
    case 'afterFileEdit':        return handleAfterFileEdit(input);
    case 'beforeSubmitPrompt':   return handleBeforeSubmitPrompt(input);
    case 'stop':                 return handleStop(input);
    default:
      // Unknown/unmapped event: never block.
      return emit({ permission: 'allow', continue: true });
  }
}

try {
  main();
} catch (e) {
  // Absolute fail-open: an adapter crash must not wedge Cursor.
  try { process.stderr.write(`[ruflo-cursor-adapter] ${e && e.stack ? e.stack : e}\n`); } catch (_) {}
  process.stdout.write(JSON.stringify({ permission: 'allow', continue: true }));
  process.exit(0);
}
