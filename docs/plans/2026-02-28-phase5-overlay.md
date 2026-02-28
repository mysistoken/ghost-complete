# Ghost Complete — Phase 5: Popup Overlay Rendering (gc-overlay)

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** A pure rendering library that takes suggestions + terminal geometry and produces ANSI byte sequences for a flicker-free popup overlay.

**Architecture:** gc-overlay writes escape sequences into a caller-provided `Vec<u8>`. It does not own stdout, manage popup visibility, or intercept keystrokes. All positioning math lives in `layout.rs`, all ANSI generation in `ansi.rs`, rendering in `render.rs`, cleanup in `cleanup.rs`. The caller (Phase 6's proxy loop) writes the buffer to stdout.

**Tech Stack:** Raw ANSI escape sequences (no crossterm for rendering — we need precise byte control for DECSET 2026). `gc-suggest::Suggestion` for input types.

---

## Context

Phases 1–4 are complete. We have a PTY proxy, terminal state tracking, command buffer parsing, and a suggestion engine. Phase 5 builds the popup renderer — a stateless library that generates ANSI sequences. Proxy integration (triggering, keystroke interception) comes in Phase 6.

---

## Architecture

**Stateless rendering** — `render_popup()` takes suggestions + geometry, returns ANSI bytes. No mutable renderer struct, no frame diffing. Each call produces a complete popup render.

**Data flow:**
```
Suggestions + OverlayState + cursor pos + screen dims
  → layout.rs computes PopupLayout (position, dimensions, above/below)
  → render.rs formats each item line (kind gutter, text, description)
  → ansi.rs wraps in synchronized output + cursor save/restore
  → Vec<u8> output ready for stdout
```

**File layout** (`crates/gc-overlay/src/`):
```
lib.rs          — public API re-exports
types.rs        — OverlayState, PopupLayout, MAX_VISIBLE
ansi.rs         — ANSI sequence helpers (cursor move, sync output, colors)
layout.rs       — compute_layout() positioning logic
render.rs       — render_popup(), format_item(), clear_popup()
```

---

## Step 1: Cargo.toml + types.rs

### 1.1 — `crates/gc-overlay/Cargo.toml`

Replace `gc-parser` and `crossterm` deps with `gc-suggest` (we need `Suggestion` and `SuggestionKind`). Remove crossterm — we write raw ANSI, not crossterm commands.

```toml
[package]
name = "gc-overlay"
version = "0.1.0"
edition = "2021"

[dependencies]
gc-suggest = { path = "../gc-suggest" }
anyhow = { workspace = true }
```

### 1.2 — `types.rs`

```rust
pub const MAX_VISIBLE: usize = 10;
pub const MIN_POPUP_WIDTH: u16 = 20;
pub const MAX_POPUP_WIDTH: u16 = 60;

#[derive(Debug, Clone)]
pub struct OverlayState {
    pub selected: usize,
    pub scroll_offset: usize,
}

impl OverlayState {
    pub fn new() -> Self {
        Self { selected: 0, scroll_offset: 0 }
    }

    /// Move selection up, scrolling if needed.
    pub fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
            if self.selected < self.scroll_offset {
                self.scroll_offset = self.selected;
            }
        }
    }

    /// Move selection down, scrolling if needed.
    pub fn move_down(&mut self, total_items: usize) {
        if self.selected + 1 < total_items {
            self.selected += 1;
            if self.selected >= self.scroll_offset + MAX_VISIBLE {
                self.scroll_offset = self.selected - MAX_VISIBLE + 1;
            }
        }
    }

    /// Reset state (e.g., when suggestions change).
    pub fn reset(&mut self) {
        self.selected = 0;
        self.scroll_offset = 0;
    }
}

#[derive(Debug, Clone)]
pub struct PopupLayout {
    pub start_row: u16,
    pub start_col: u16,
    pub width: u16,
    pub height: u16,
    pub renders_above: bool,
}
```

### Tests
- `test_move_down_increments` — selected goes from 0 to 1
- `test_move_up_decrements` — selected goes from 1 to 0
- `test_move_up_at_zero_stays` — selected stays at 0
- `test_move_down_at_end_stays` — selected stays at total-1
- `test_scroll_offset_on_move_down` — offset adjusts when selected >= offset + MAX_VISIBLE
- `test_scroll_offset_on_move_up` — offset adjusts when selected < offset
- `test_reset` — selected and offset both go to 0

---

## Step 2: ansi.rs — ANSI sequence helpers

Low-level byte writers. All functions append to a `&mut Vec<u8>`.

```rust
use std::io::Write;

/// Begin synchronized output (Ghostty DECSET 2026).
pub fn begin_sync(buf: &mut Vec<u8>) {
    let _ = buf.write_all(b"\x1b[?2026h");
}

/// End synchronized output.
pub fn end_sync(buf: &mut Vec<u8>) {
    let _ = buf.write_all(b"\x1b[?2026l");
}

/// Save cursor position (DECSC).
pub fn save_cursor(buf: &mut Vec<u8>) {
    let _ = buf.write_all(b"\x1b7");
}

/// Restore cursor position (DECRC).
pub fn restore_cursor(buf: &mut Vec<u8>) {
    let _ = buf.write_all(b"\x1b8");
}

/// Move cursor to absolute position (1-indexed for ANSI).
pub fn move_to(buf: &mut Vec<u8>, row: u16, col: u16) {
    let _ = write!(buf, "\x1b[{};{}H", row + 1, col + 1);
}

/// Set reverse video (for selected item).
pub fn reverse_video(buf: &mut Vec<u8>) {
    let _ = buf.write_all(b"\x1b[7m");
}

/// Set dim text (for descriptions).
pub fn dim(buf: &mut Vec<u8>) {
    let _ = buf.write_all(b"\x1b[2m");
}

/// Set bold text.
pub fn bold(buf: &mut Vec<u8>) {
    let _ = buf.write_all(b"\x1b[1m");
}

/// Reset all attributes.
pub fn reset(buf: &mut Vec<u8>) {
    let _ = buf.write_all(b"\x1b[0m");
}

/// Erase from cursor to end of line.
pub fn erase_to_eol(buf: &mut Vec<u8>) {
    let _ = buf.write_all(b"\x1b[K");
}
```

### Tests
- `test_begin_sync` — output contains `\x1b[?2026h`
- `test_end_sync` — output contains `\x1b[?2026l`
- `test_move_to_one_indexed` — move_to(0, 0) produces `\x1b[1;1H`
- `test_move_to_arbitrary` — move_to(5, 10) produces `\x1b[6;11H`
- `test_save_restore_cursor` — correct escape codes
- `test_reverse_video` — correct escape code
- `test_reset` — correct escape code

---

## Step 3: layout.rs — Popup positioning

```rust
use crate::types::{PopupLayout, MAX_VISIBLE, MIN_POPUP_WIDTH, MAX_POPUP_WIDTH};
use gc_suggest::Suggestion;

pub fn compute_layout(
    suggestions: &[Suggestion],
    scroll_offset: usize,
    cursor_row: u16,
    cursor_col: u16,
    screen_rows: u16,
    screen_cols: u16,
) -> PopupLayout {
    let visible_count = suggestions.len().min(MAX_VISIBLE);
    let height = visible_count as u16;

    // Compute width from visible suggestions
    let content_width = suggestions
        .iter()
        .skip(scroll_offset)
        .take(MAX_VISIBLE)
        .map(|s| item_display_width(s))
        .max()
        .unwrap_or(MIN_POPUP_WIDTH as usize);
    let width = (content_width as u16).clamp(MIN_POPUP_WIDTH, MAX_POPUP_WIDTH.min(screen_cols));

    // Vertical: prefer below cursor, above if not enough space
    let space_below = screen_rows.saturating_sub(cursor_row + 1);
    let renders_above = space_below < height;
    let start_row = if renders_above {
        cursor_row.saturating_sub(height)
    } else {
        cursor_row + 1
    };

    // Horizontal: start at cursor col, shift left if overflows
    let start_col = if cursor_col + width > screen_cols {
        screen_cols.saturating_sub(width)
    } else {
        cursor_col
    };

    PopupLayout { start_row, start_col, width, height, renders_above }
}

/// Calculate display width for a single suggestion (kind gutter + text + description).
fn item_display_width(suggestion: &Suggestion) -> usize {
    // " X text  description " — gutter(3) + text + gap(2) + desc + padding(1)
    let text_len = suggestion.text.len();
    let desc_len = suggestion.description.as_ref().map(|d| d.len() + 2).unwrap_or(0);
    3 + text_len + desc_len + 1
}
```

### Tests
- `test_popup_below_cursor` — cursor at row 5 of 24, popup starts at row 6
- `test_popup_above_cursor` — cursor at row 22 of 24, popup renders above
- `test_popup_shifts_left` — cursor at col 70 of 80, popup shifts left to fit
- `test_popup_at_top_left` — cursor at (0, 0), popup below at (1, 0)
- `test_width_clamped_min` — short suggestions still get MIN_POPUP_WIDTH
- `test_width_clamped_max` — very long suggestions capped at MAX_POPUP_WIDTH
- `test_height_capped_at_max_visible` — 50 suggestions still show MAX_VISIBLE rows

---

## Step 4: render.rs — Popup rendering + cleanup

The main event. Combines ansi.rs helpers with layout.rs positioning.

### `render_popup()`

```rust
pub fn render_popup(
    buf: &mut Vec<u8>,
    suggestions: &[Suggestion],
    state: &OverlayState,
    cursor_row: u16,
    cursor_col: u16,
    screen_rows: u16,
    screen_cols: u16,
) -> PopupLayout {
    let layout = layout::compute_layout(
        suggestions, state.scroll_offset,
        cursor_row, cursor_col, screen_rows, screen_cols,
    );

    ansi::begin_sync(buf);
    ansi::save_cursor(buf);

    let visible = &suggestions[state.scroll_offset..][..layout.height as usize];
    for (i, suggestion) in visible.iter().enumerate() {
        let row = layout.start_row + i as u16;
        let is_selected = state.scroll_offset + i == state.selected;

        ansi::move_to(buf, row, layout.start_col);

        if is_selected {
            ansi::reverse_video(buf);
        }

        format_item(buf, suggestion, layout.width, is_selected);

        ansi::reset(buf);
    }

    ansi::restore_cursor(buf);
    ansi::end_sync(buf);

    layout
}
```

### `format_item()`

```rust
fn format_item(buf: &mut Vec<u8>, s: &Suggestion, width: u16, is_selected: bool) {
    use std::io::Write;
    use gc_suggest::SuggestionKind;

    // Kind gutter (1 char + space)
    let kind_char = match s.kind {
        SuggestionKind::Command => 'C',
        SuggestionKind::Subcommand => 'S',
        SuggestionKind::Flag => 'F',
        SuggestionKind::FilePath => 'f',
        SuggestionKind::Directory => 'd',
        SuggestionKind::GitBranch => 'B',
        SuggestionKind::GitTag => 'T',
        SuggestionKind::GitRemote => 'R',
        SuggestionKind::History => 'H',
    };

    let _ = write!(buf, " {kind_char} ");

    // Text
    let text = &s.text;
    let _ = write!(buf, "{text}");

    // Description (if room)
    let used = 3 + text.len();
    let remaining = (width as usize).saturating_sub(used + 1);

    if let Some(desc) = &s.description {
        if remaining > 4 {
            let padding = remaining.saturating_sub(desc.len());
            for _ in 0..padding.min(remaining) {
                let _ = buf.write_all(b" ");
            }
            if !is_selected {
                ansi::dim(buf);
            }
            let truncated: String = desc.chars().take(remaining).collect();
            let _ = write!(buf, "{truncated}");
            if !is_selected {
                ansi::reset(buf);
                if is_selected { ansi::reverse_video(buf); }
            }
        } else {
            // Pad remaining space
            for _ in 0..(remaining + 1) {
                let _ = buf.write_all(b" ");
            }
        }
    } else {
        // Pad remaining space
        for _ in 0..(remaining + 1) {
            let _ = buf.write_all(b" ");
        }
    }
}
```

Actually — the description formatting logic above is getting fiddly with attribute toggling mid-line. Let me simplify. For selected items, the whole line is reverse video including the description. For non-selected items, text is normal and description is dim. Much cleaner:

```rust
fn format_item(buf: &mut Vec<u8>, s: &Suggestion, width: u16, is_selected: bool) {
    use std::io::Write;
    use gc_suggest::SuggestionKind;

    let kind_char = match s.kind {
        SuggestionKind::Command => 'C',
        SuggestionKind::Subcommand => 'S',
        SuggestionKind::Flag => 'F',
        SuggestionKind::FilePath => 'f',
        SuggestionKind::Directory => 'd',
        SuggestionKind::GitBranch => 'B',
        SuggestionKind::GitTag => 'T',
        SuggestionKind::GitRemote => 'R',
        SuggestionKind::History => 'H',
    };

    // Build the line content as a string first, then pad to width
    let text = &s.text;
    let desc_part = s.description.as_deref().unwrap_or("");

    let gutter = format!(" {kind_char} ");
    let gutter_text_len = gutter.len() + text.len();
    let max_desc_len = (width as usize).saturating_sub(gutter_text_len + 2 + 1); // 2 gap + 1 trailing

    let _ = write!(buf, "{gutter}{text}");

    if !desc_part.is_empty() && max_desc_len > 2 {
        let _ = write!(buf, "  ");
        if !is_selected { ansi::dim(buf); }
        let truncated: String = desc_part.chars().take(max_desc_len).collect();
        let _ = write!(buf, "{truncated}");
        if !is_selected { ansi::reset(buf); }
        // Pad remaining
        let used = gutter_text_len + 2 + truncated.len();
        let pad = (width as usize).saturating_sub(used);
        for _ in 0..pad { let _ = buf.write_all(b" "); }
    } else {
        let pad = (width as usize).saturating_sub(gutter_text_len);
        for _ in 0..pad { let _ = buf.write_all(b" "); }
    }
}
```

### `clear_popup()`

```rust
pub fn clear_popup(buf: &mut Vec<u8>, layout: &PopupLayout) {
    ansi::begin_sync(buf);
    ansi::save_cursor(buf);

    for row_offset in 0..layout.height {
        let row = layout.start_row + row_offset;
        ansi::move_to(buf, row, layout.start_col);
        ansi::erase_to_eol(buf);
    }

    ansi::restore_cursor(buf);
    ansi::end_sync(buf);
}
```

Wait — `erase_to_eol` erases from cursor to end of line, which could erase content to the RIGHT of the popup that doesn't belong to us. We need to write exactly `width` spaces instead:

```rust
pub fn clear_popup(buf: &mut Vec<u8>, layout: &PopupLayout) {
    use std::io::Write;
    ansi::begin_sync(buf);
    ansi::save_cursor(buf);

    for row_offset in 0..layout.height {
        let row = layout.start_row + row_offset;
        ansi::move_to(buf, row, layout.start_col);
        for _ in 0..layout.width {
            let _ = buf.write_all(b" ");
        }
    }

    ansi::restore_cursor(buf);
    ansi::end_sync(buf);
}
```

### Tests
- `test_render_produces_sync_wrapper` — output starts with `\x1b[?2026h`, ends with `\x1b[?2026l`
- `test_render_saves_restores_cursor` — contains `\x1b7` and `\x1b8`
- `test_render_positions_at_layout` — output contains cursor moves to correct rows
- `test_selected_item_has_reverse_video` — `\x1b[7m` appears in output
- `test_format_item_shows_kind_gutter` — " S checkout" for subcommand
- `test_format_item_truncates_description` — long desc truncated to fit width
- `test_clear_writes_spaces` — no `\x1b[K` (erase to EOL), only space characters
- `test_clear_correct_dimensions` — correct number of rows cleared
- `test_render_with_scroll_offset` — scroll_offset=2 skips first 2 items
- `test_render_empty_suggestions` — no crash, layout has height 0

---

## Step 5: lib.rs — Wiring

```rust
mod ansi;
mod layout;
mod render;
pub mod types;

pub use render::{clear_popup, render_popup};
pub use types::{OverlayState, PopupLayout, MAX_VISIBLE};
```

---

## Step 6: Verification

1. `cargo build` — clean compile
2. `cargo clippy --all-targets` — no warnings
3. `cargo fmt --check` — clean
4. `cargo test -p gc-overlay` — all tests pass
5. `cargo test` — full workspace green (98 existing + new gc-overlay tests)

---

## Files to create/modify

| File | Action |
|------|--------|
| `crates/gc-overlay/Cargo.toml` | Modify — replace deps with gc-suggest |
| `crates/gc-overlay/src/lib.rs` | Rewrite — module declarations + re-exports |
| `crates/gc-overlay/src/types.rs` | Create — OverlayState, PopupLayout, constants |
| `crates/gc-overlay/src/ansi.rs` | Create — ANSI sequence helpers |
| `crates/gc-overlay/src/layout.rs` | Create — compute_layout() positioning |
| `crates/gc-overlay/src/render.rs` | Create — render_popup(), format_item(), clear_popup() |

## Design decisions

**Write to Vec<u8>, not stdout** — Keeps the library pure and testable. The PTY proxy loop (Phase 6) owns stdout and controls when bytes get flushed. This also allows the caller to batch overlay output with shell output in a single write for minimal syscalls.

**Raw ANSI, not crossterm** — crossterm's `Command` trait writes directly to a `Write` implementor. We need precise byte-level control for DECSET 2026 (synchronized output), which crossterm doesn't expose as a command. Raw escape sequences are also faster (no trait dispatch) and exactly what Ghostty expects.

**No frame diffing** — Each render call produces a complete popup. No previous-frame tracking, no dirty rectangles. For 10 lines of text this is <1ms and simpler than maintaining diff state.

**erase_to_eol avoided in cleanup** — Writing exact-width spaces instead of `\x1b[K` prevents accidentally erasing content to the right of the popup that belongs to the shell output.
