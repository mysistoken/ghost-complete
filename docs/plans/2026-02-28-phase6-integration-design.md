# Phase 6: PTY Integration — Design Document

**Goal:** Wire gc-buffer, gc-suggest, and gc-overlay into the gc-pty event loop so keystrokes trigger suggestions and render an interactive popup.

**Approach:** Monolithic proxy refactor — add `input.rs` (key parsing) and `handler.rs` (popup state machine) to gc-pty. Modify Task A in proxy.rs to route keystrokes through InputHandler instead of blind forwarding.

---

## Architecture

Three new/modified files in `crates/gc-pty/src/`:

| File | Role |
|------|------|
| `input.rs` | Parse raw stdin bytes into `KeyEvent` enum |
| `handler.rs` | Popup state machine — trigger, navigate, accept, dismiss |
| `proxy.rs` | Modified Task A to use InputHandler |

### Data Flow

```
stdin bytes
  → input::parse_keys(&buf[..n])
  → Vec<KeyEvent>
  → for each key: handler.process_key(key, &parser, &mut pty_writer, &mut stdout)
       │
       ├─ popup hidden: forward to PTY, check trigger conditions
       │    → if triggered: parse_command_context → suggest_sync → render_popup
       │
       └─ popup visible: intercept nav/accept/dismiss
            → Tab: accept (backspace partial + type suggestion)
            → ↑/↓: navigate, re-render
            → Escape: clear popup, dismiss
            → Printable: forward to PTY, re-trigger
            → Enter: accept + forward Enter
```

---

## Key Parsing (`input.rs`)

Minimal byte-level parser. No crossterm event reader (it takes over stdin).

| Bytes | KeyEvent |
|-------|----------|
| `0x09` | Tab |
| `0x0D` | Enter |
| `0x1B` alone | Escape |
| `0x1B [ A` | ArrowUp |
| `0x1B [ B` | ArrowDown |
| `0x1B [ C` | ArrowRight |
| `0x1B [ D` | ArrowLeft |
| `0x00` | CtrlSpace |
| `0x20-0x7E` | Printable(char) |
| anything else | Raw(bytes) |

Escape disambiguation: if `0x1B` is followed by `[` in the same read buffer, it's a CSI sequence. If `0x1B` is alone at end of buffer, it's standalone Escape. Unknown CSI sequences become `Raw(bytes)`.

---

## Input Handler (`handler.rs`)

### State

```rust
pub struct InputHandler {
    engine: SuggestionEngine,
    overlay: OverlayState,
    suggestions: Vec<Suggestion>,
    last_layout: Option<PopupLayout>,
    visible: bool,
}
```

Borrows `TerminalParser` (via Arc<Mutex>), `pty_writer`, and `stdout` during `process_key()`.

### Popup Hidden Mode

Forward all keys to PTY. After forwarding, check trigger conditions:

- Printable char after known command position → trigger
- Space at word_index=0 after a command → trigger
- `/`, `.` typed → trigger (path completion)
- `-` typed → trigger (flag completion)
- Ctrl+Space → force trigger

### Popup Visible Mode

| Key | Action |
|-----|--------|
| Tab | Accept selected, dismiss |
| Enter | Accept selected, send Enter to PTY, dismiss |
| ↑ | Navigate up, re-render |
| ↓ | Navigate down, re-render |
| Escape | Clear popup, dismiss |
| Printable | Forward to PTY, re-trigger with updated buffer |
| ←/→ | Dismiss popup, forward to PTY |
| Other | Forward to PTY, dismiss popup |

### Trigger Pipeline

1. Lock `TerminalParser` → read command buffer + cursor + CWD + screen dims
2. `parse_command_context(buffer, cursor)` → `CommandContext`
3. `engine.suggest_sync(&ctx, &cwd)` → `Vec<Suggestion>`
4. If non-empty → `render_popup()` to stdout, store layout, set visible=true
5. If empty → clear if was visible

### Accept Mechanism (Synthetic Keystrokes)

1. Send `\x7F` (backspace) × `current_word.len()` to PTY writer
2. Send suggestion text bytes to PTY writer
3. Clear popup from screen, reset overlay state

---

## Proxy Modifications

### Task A

Replace blind forwarding with:
```
read stdin → parse_keys → handler.process_key() for each key
```

Handler writes popup ANSI directly to its own stdout handle. Synchronized output (DECSET 2026) prevents tearing.

### Task B

Unchanged. Shell output flows through TerminalParser and to stdout as before. The popup renders on top — when the handler re-triggers after forwarding a printable key, it re-renders the popup fresh.

### SIGWINCH

If popup visible: clear old layout, re-render with new screen dimensions.

---

## Dependencies

Add to `gc-pty/Cargo.toml`:
- `gc-buffer`
- `gc-suggest`
- `gc-overlay`

---

## Testing

- `input.rs` — unit tests with known byte sequences (Tab, arrows, escape, printable, multi-key buffers)
- `handler.rs` — unit tests with Vec<u8> buffers as mock stdout/pty, fake TerminalParser state, test engine with known specs
- `proxy.rs` — manual testing only (I/O integration)

---

## Not In Scope (YAGNI)

- Debounce timer — trigger immediately, add delay later if needed
- Suggestion caching — re-query every trigger (<50ms)
- gc-config integration — hardcoded keybindings, Phase 7 handles config
- Kitty keyboard protocol — standard ANSI only
