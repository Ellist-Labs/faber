# Faber UI Design Spec — "Focused Glass"

> Authoritative design reference for the UI/UX redesign. Every measurement, color value,
> interaction rule, and GPUI mapping is stated here. Implement exactly what is written;
> do not infer or substitute. Where a GPUI limitation prevents the ideal, the fallback
> is also specified.

---

## 1. Design Philosophy

**Core principle:** Chrome hides; glass surfaces emerge on demand.

- No persistent sidebar. The sidebar is a glass overlay summoned on `⌘1`, dismissed on focus loss.
- No activity bar. All actions live in the command palette (`⌘K`), not icon strips.
- Problems and References are **editor page-tabs** — they replace the code area in the tab row,
  not bottom panels.
- The bottom panel is for the **terminal only** (and future infrastructure tabs). No drag handle.
  No close button. Toggle with `⌘J`.
- Every persistent surface must fit on a 13-inch MacBook screen opened next to a simulator.

---

## 2. Token System — OLED Black Theme

This is the only theme for this design pass. Replace all existing Catppuccin-derived palette
values with the following. Token names map to the existing `SemanticColors` struct in
`crates/faber-theme/src/lib.rs`.

### 2.1 Surface colors

| Token | Hex / RGBA | Usage |
|---|---|---|
| `bg` | `#000000` | Window background, active tab background |
| `bg_elevated` | `#0D0D0D` | Title bar, tab bar, bottom panel, status bar |
| `bg_raised` | `#1A1A1A` | Hover rows, badge backgrounds, item icon fills |
| `bg_sunken` | `#080808` | Gutter background |
| `bg_overlay` | `rgba(0,0,0,0.76)` | Glass surface base (see §3) |

### 2.2 Text colors

| Token | Hex | Usage |
|---|---|---|
| `text` | `#FFFFFF` | Primary text |
| `text_muted` | `#888888` | Secondary labels, status bar items, gutter line numbers |
| `text_subtle` | `#555555` | Placeholder text, disabled state, comment-tier info |

### 2.3 Border colors

| Token | RGBA | Usage |
|---|---|---|
| `border` | `rgba(255,255,255,0.07)` | Default dividers, panel borders, tab separators |
| `border_focus` | `rgba(255,255,255,0.12)` | Glass surface borders, focused inputs |
| `separator` | `rgba(255,255,255,0.07)` | Alias for `border`; use on internal dividers |

### 2.4 Accent

| Token | Value | Usage |
|---|---|---|
| `accent` | `#5E5CE6` | Cursor, active tab indicator, active keybinding chips, action buttons |
| `accent_hover` | `#7472E8` | Hover state of accent-colored elements |
| `accent_muted` | `rgba(94,92,230,0.18)` | Selected autocomplete row, focused palette item, selection highlight |

### 2.5 Editor-specific

| Token | Value | Usage |
|---|---|---|
| `cursor` | `#5E5CE6` | Blinking caret |
| `selection` | `rgba(94,92,230,0.22)` | Text selection background |
| `line_highlight` | `rgba(255,255,255,0.03)` | Current line background in editor and gutter |
| `gutter` | `#080808` | Gutter column background |
| `gutter_active` | `rgba(255,255,255,0.03)` | Gutter line number highlight on cursor line |
| `word_highlight` | `rgba(94,92,230,0.15)` | Other occurrences of selected word |
| `match_bg` | `rgba(94,92,230,0.25)` | Search match background |
| `match_active` | `rgba(94,92,230,0.45)` | Active (focused) search match |
| `dirty` | `#5E5CE6` | Unsaved-changes dot on tab |

### 2.6 Status colors

| Token | Hex | Usage |
|---|---|---|
| `error` | `#FF453A` | Error diagnostics, error badge text |
| `warning` | `#FF9F0A` | Warning diagnostics |
| `success` | `#30D158` | LSP connected, build success in terminal |
| `info` | `#5E5CE6` | Informational; reuses accent |

### 2.7 Syntax palette

Applied to `SyntaxTheme` in `faber-theme`. All are exact hex values.

| Role | Token name | Color | Style |
|---|---|---|---|
| Keyword | `keyword` | `#CF8EF4` | italic |
| Function/method | `function` | `#82AAFF` | — |
| Type/class/struct | `type_` | `#FFCB6B` | — |
| String | `string` | `#C3E88D` | — |
| Number/constant | `number` | `#F78C6C` | — |
| Comment | `comment` | `#546E7A` | italic |
| Property/field | `property` | `#B2CCD6` | — |
| Operator | `operator` | `#89DDFF` | — |
| Punctuation | `punctuation` | `rgba(255,255,255,0.28)` | — |
| Attribute/decorator | `attribute` | `#FF9F0A` | — |

---

## 3. Glass Material

Glass is applied to all overlaid, transient surfaces: sidebar, command palette, hover popover,
autocomplete dropdown.

### 3.1 Ideal recipe (CSS reference, used in prototype)

```
background: rgba(0,0,0,0.76)
backdrop-filter: blur(28px) saturate(160%)
border: 1px solid rgba(255,255,255,0.12)   ← border_focus token
box-shadow: 0 10px 40px rgba(0,0,0,0.65), 0 4px 16px rgba(0,0,0,0.5)
```

### 3.2 GPUI fallback

GPUI does not expose `backdrop-filter`. Use this approximation — the layered shadow
still reads as glass against the dark background:

```rust
.bg(gpui::rgba(0x00000082))   // ~0.51 opacity but combined with shadow reads darker
.border_1()
.border_color(t.border_focus) // rgba(255,255,255,0.12)
.shadow(vec![
    gpui::BoxShadow { color: gpui::rgba(0x000000A6), blur: px(40.), spread: px(0.), offset: point(px(0.), px(10.)) },
    gpui::BoxShadow { color: gpui::rgba(0x00000080), blur: px(16.), spread: px(0.), offset: point(px(0.), px(4.)) },
])
```

Until GPUI supports backdrop blur, increase the glass `bg` opacity to `0x000000C2` (≈0.76)
so the surface is visually opaque enough against code content.

### 3.3 Glass border radius by surface

| Surface | Radius |
|---|---|
| Command palette | `14px` |
| Sidebar | `0` (full-height overlay) |
| Hover popover | `10px` |
| Autocomplete dropdown | `10px` |

---

## 4. Layout Architecture

```
Window (flex col, 100vw × 100vh)
├── TitleBar          38px  flex-shrink:0  (native macOS traffic lights)
├── TabBar            36px  flex-shrink:0
└── Mid               flex:1  flex-col  overflow:hidden
    ├── EditorRow     flex:1  flex-row  overflow:hidden  (position:relative for overlays)
    │   ├── Gutter    54px   flex-shrink:0
    │   ├── CodeArea  flex:1  overflow:auto
    │   ├─ ─ ProblemPage  flex:1  (replaces Gutter+CodeArea when active)
    │   ├─ ─ ReferencePage  flex:1  (same)
    │   ├─ ─ WelcomeScreen  flex:1  (when no project open)
    │   │
    │   │  ── Overlays (position:absolute, z-indexed) ──
    │   ├─ ─ Sidebar           z=40  left:0
    │   ├─ ─ CommandPalette    z=60  centered
    │   ├─ ─ HoverPopover      z=50  caret-anchored
    │   └─ ─ AutocompleteDropdown  z=55  caret-anchored
    │
    ├── BottomPanel   0px → 168px  border-radius:10 10 0 0  (terminal only)
    └── StatusBar     24px  flex-shrink:0
```

---

## 5. Components

### 5.1 Title Bar

- Height: `38px`
- Background: `bg_elevated` (`#0D0D0D`)
- Border-bottom: `1px border`
- Content: macOS native traffic lights (left-aligned, 14px from left edge), window title centered
- Window title: `12px`, `text_muted`, format `"{filename} — {project_name}"`
  - When welcome screen is active: just `"Faber"`
  - When a page-tab is active: `"Problems — {project_name}"` or `"References — {project_name}"`
- No custom close/minimize/maximize — use native macOS controls only

### 5.2 Tab Bar

- Height: `36px`
- Background: `bg_elevated`
- Border-bottom: `1px border`
- Overflow: hidden (no scrolling; tabs shrink proportionally if needed)
- Tab separator: `1px border` right-side on each tab

**Tab anatomy:**

```
[language dot 7px] [filename] [dirty dot OR close ×]
```

- Tab padding: `0 10px 0 12px`
- Gap between dot and name: `7px`
- Min-width: `126px`, max-width: `200px`
- Font: `12px`, default color `text_muted`
- Hover: background `rgba(255,255,255,0.04)`, color `text_muted`
- Active tab: background `bg` (`#000000`), color `text`, accent bottom bar `2px accent` at bottom edge
- Language dot: `7px` diameter circle, language-specific color (Swift: `#F05138`, Rust: `#DEA584`)
- Dirty dot (unsaved): `6px` diameter, color `dirty` (`#5E5CE6`); hidden on hover, replaced by close ×
- Close × button: `14×14px`, `border-radius:4px`, `10px` font; opacity 0 at rest, visible on tab hover; background `rgba(255,255,255,0.1)` on its own hover

**Special tab kinds (Problems, References):**

- Shown only when the page is open (dynamically inserted/removed from tab row)
- Language dot replaced by a colored status dot: `error` color for Problems, `text_muted` for References
- Error badge: small pill right of name — `background: rgba(255,69,58,0.14)`, `color: error`, `border-radius: 5px`, `padding: 1px 6px`, `font-size: 10px`
- Clicking the × on these tabs: closes the page-tab, restores last active file tab

### 5.3 Editor Area

**Gutter:**
- Width: `54px`
- Background: `bg_sunken` (`#080808`)
- Border-right: `1px border`
- Line numbers: right-aligned with `13px` right padding
- Font: monospace, `12px`, color `text_subtle`
- Current line number: color `text_muted`
- Padding top: `16px`

**Code scroll area:**
- Background: `bg` (`#000000`)
- Padding: `16px 20px 40px`
- Font: monospace, `13px`, line-height `21px`
- Tab size: `4`
- Current line highlight: background `line_highlight` on full line span

**Cursor:**
- `2px` wide, height `14px`, color `accent`, `border-radius: 1px`
- Blink: 1.1s step-end animation, 50% duty cycle

### 5.4 Sidebar Overlay

- Width: `240px`
- Position: `absolute`, `top:0 bottom:0 left:0`, `z-index: 40`
- Applies glass material (§3)
- Border-right: `1px border_focus`
- Default state: translated `translateX(-100%)`, no shadow
- Open state: translated `translateX(0)`, box-shadow `16px 0 48px rgba(0,0,0,0.6)`
- Transition: `220ms cubic-bezier(0.25, 0.46, 0.45, 0.94)`
- Triggered by: `⌘1`; auto-dismissed when editor receives click or `Esc`

**Header strip:**
- Height: `30px`
- Border-bottom: `1px border`
- Label: `10.5px`, weight `600`, letter-spacing `0.08em`, uppercase, `text_muted`
- Text: `"EXPLORER"` (or panel name)
- Padding: `0 14px`

**File tree rows:**
- Height: `25px`
- Padding: `0 8px`
- Margin: `0 5px` (so row is inset from sidebar edges)
- Border-radius: `7px`
- Font: `12.5px`, color `text_muted`
- Hover: background `rgba(255,255,255,0.06)`, color `text`
- Selected: background `accent_muted`, color `text`
- Directory row: color `text_subtle`, weight `500`
- Indent levels: `+16px` padding-left per depth level (starting at `8px` for root)
- Language dot: `7px` (same as tab bar)

### 5.5 Command Palette

- Triggered by: `⌘K`; dismissed by `Esc` or clicking outside
- Position: centered horizontally in the editor row, `padding-top: 58px` from top of editor row
- Width: `540px` (or `calc(100% - 36px)` if viewport is smaller)
- Applies glass material (§3), `border-radius: 14px`
- Entry animation: `translateY(-12px) scale(0.97)` → `translateY(0) scale(1)`, `220ms cubic-bezier(0.34, 1.36, 0.64, 1)`
- Overlay scrim: no color tint; palette sits directly over editor with glass providing separation

**Search input row:**
- Height: `~45px` (13px padding top/bottom + 19px line-height)
- Border-bottom: `1px border`
- Padding: `13px 15px`
- Icon: search `⌕`, `14px`, `text_muted`, left
- Input: `14px`, color `text`, placeholder color `text_subtle`
- Caret: `accent`

**Section groups:**
- Section label: `10px`, weight `600`, letter-spacing `0.08em`, uppercase, `text_subtle`, padding `5px 14px 3px`
- Sections separated by `1px border` top
- Between sections: `4px 0` padding

**Result items:**
- Height: `~34px` (`6px 8px` padding)
- Margin: `1px 5px`
- Border-radius: `8px`
- Focused/selected: background `accent_muted`
- Hover: background `rgba(255,255,255,0.06)`
- Layout: `[icon 20×20] [name flex:1] [meta] [keybinding]`
- Icon: `20×20px`, `border-radius: 6px`, background `bg_raised`, centered content `10px`
- Name: `13px`, `text`
- Meta (file path, line number): `11px`, `text_muted`
- Keybinding chips: see §5.11

### 5.6 Hover Popover

- Triggered by: `300ms` dwell on a token; dismissed on cursor move or `Esc`
- Position: anchored below caret (or above if insufficient space), offset `8px` from baseline
- Width: `316px` (max `560px` — clamp to viewport)
- Applies glass material (§3), `border-radius: 10px`
- Padding: `13px 15px`
- Max-height: `320px`, overflow-y: auto

**Content layout:**
```
[type signature — monospace 12px, type color]
[horizontal rule 1px border]
[documentation — 12px, text_muted, line-height 1.65]
```

- Inline `code` in docs: `11px` monospace, color `property`, background `bg_raised`, `border-radius: 4px`, `padding: 1px 4px`

### 5.7 Autocomplete Dropdown

- Triggered by: typing a character or `.` (LSP `textDocument/completion`)
- Position: anchored `8px` below caret bottom, aligned to caret left; adjusts horizontally if near viewport edge
- Width: `336px`
- Max visible items: `6` (max-height `~168px`); scrollable, scrollbar hidden
- Applies glass material (§3), `border-radius: 10px`
- Inner padding: `4px 0` top/bottom

**Item anatomy:**

```
[kind badge 17×17] [name flex:1] [type annotation right-aligned]
```

- Item height: `27px` (`5px` padding × 2 + `17px` content)
- Item padding: `5px 10px`
- Gap: `8px`
- Hover: background `rgba(255,255,255,0.06)`
- Selected (arrow-key focus): background `accent_muted`

**Kind badge:**
- Size: `17×17px`, `border-radius: 4px`
- Font: `9px`, weight `700`, sans-serif (not monospace)
- Letter shown and its colors:

| Kind | Letter | Background | Text color |
|---|---|---|---|
| Struct | S | `rgba(255,203,107,0.13)` | `#FFCB6B` (type token) |
| Class | C | `rgba(130,170,255,0.13)` | `#82AAFF` (function token) |
| Function | F | `rgba(130,170,255,0.11)` | `#82AAFF` |
| Method | M | `rgba(130,170,255,0.10)` | `#82AAFF` |
| Property | P | `rgba(178,204,214,0.12)` | `#B2CCD6` (property token) |
| Keyword | K | `rgba(207,142,244,0.12)` | `#CF8EF4` (keyword token) |
| Variable | V | `rgba(255,255,255,0.06)` | `text_muted` |
| Module | M | `rgba(137,221,255,0.10)` | `#89DDFF` (operator token) |

**Item name:** `12.5px` monospace, `text`

**Type annotation:** `11px` sans-serif, `text_subtle`, max-width `110px`, truncated

**Keyboard:**
- `↑ ↓` — move selection
- `Tab` / `Enter` — accept selected item
- `Esc` — dismiss without inserting
- Typing continues to filter

### 5.8 Problems Page-Tab

When the Problems tab is activated, the gutter and code area are hidden and this view fills the entire editor row (`.ea`). The tab bar stays visible.

**Banner:**
- Padding: `16px 20px 13px`
- Border-bottom: `1px border`
- Title: `"Problems"`, `13px`, weight `600`, `text`
- Summary pills: `[error count]` and `[warning count]`
  - Pill: `border-radius: 20px`, `padding: 3px 9px`, `font-size: 11px`, weight `500`
  - Error pill: background `rgba(255,69,58,0.12)`, color `error`
  - Warning pill: background `rgba(255,159,10,0.10)`, color `warning`
  - Each pill has a `5px` colored dot left of the count label

**File groups:**
- Padding: `8px 12px 4px`
- File header row: `flex`, `padding: 7px 8px`, `border-radius: 7px`, `12px`, weight `500`, `text_muted`
  - Language dot left, file count badge right
  - File count badge: background `bg_raised`, `border-radius: 5px`, `padding: 1px 6px`, `font-size: 10px`
- Groups separated by `1px border` top

**Diagnostic rows:**
- Indent: `24px` left padding (inside the file group)
- Padding: `7px 8px`
- Border-radius: `8px`
- Hover: background `rgba(255,255,255,0.05)`
- Layout: `[severity pill] [message + location]`
  - Severity pill: `font-size: 9.5px`, weight `600`, uppercase, letter-spacing `0.04em`, `border-radius: 5px`, `padding: 2px 6px`, margin-top `2px`
    - Error: background `rgba(255,69,58,0.14)`, color `error`
    - Warning: background `rgba(255,159,10,0.13)`, color `warning`
  - Message: `12.5px`, `text`, line-height `1.45`
  - Location: `11px` monospace, `text_subtle`, margin-top `2px` — format `"line {n}, col {n} · {source}"`

**Click on a row:** navigate to that file + line, close the Problems tab, restore the file tab.

### 5.9 References Page-Tab

Same structural pattern as Problems (§5.8) with these differences:

- Tab title: `"References"`; tab dot color: `text_muted`
- No severity pills — rows show file path + line preview + column
- Banner: `"References"` title + `"{n} results"` count in `text_muted`
- Row layout: `[file icon] [line preview] [location right-aligned]`
- Triggered by: `Shift+F12`

### 5.10 Bottom Panel — Terminal

- Height transitions: `0px` (collapsed) → `168px` (open)
- Transition: `height 200ms cubic-bezier(0.25, 0.46, 0.45, 0.94)`
- Background: `bg_elevated`
- Border-top: `1px border`
- Border-radius: `10px 10px 0 0` (top corners only)
- **No drag handle. No close button.**
- Triggered by: `⌘J` (toggle open/close)

**Tab strip inside panel:**
- Height: `34px`
- Border-bottom: `1px border`
- Padding: `0 6px`
- Tab: `11.5px`, color `text_muted`, `padding: 0 9px`, `border-bottom: 2px transparent`
- Active tab: color `text`, border-bottom `2px accent`
- Tabs: Terminal, Output (future: Debugger)

**Terminal body:**
- Padding: `10px 18px`
- Font: monospace, `12.5px`, line-height `1.8`
- Prompt color: `accent`
- Success output: `success` (`#30D158`)
- Default output: `text`
- Cursor: `6px × 13px` block, `rgba(255,255,255,0.5)`, `border-radius: 1.5px`, blink animation

### 5.11 Status Bar

- Height: `24px`
- Background: `bg_elevated`
- Border-top: `1px border`
- Font: `11px`, color `text_muted`
- Items separated by `1px border` (not gap)
- Padding per item: `2px 7px`
- Item border-radius: `6px`
- Item hover: background `bg_raised`, color `text_muted`
- Clickable items hover: color `text`

**Left section (language + LSP status):**
1. Language name (e.g., `"swift-lang"`) — status dot `accent` color
2. LSP server name (e.g., `"sourcekit-lsp"`) — status dot `success` when connected
3. Error count — status dot `error`; click opens Problems page-tab
4. Warning count — status dot `warning`; click opens Problems page-tab

**Right section (editor state):**
1. `"Ln {n}, Col {n}"` — cursor position; hidden when no file is open
2. `"UTF-8"` — file encoding
3. `"Spaces: 4"` or `"Tabs: N"` — indent mode

**Status dot:** `6px` diameter circle, inline-flex, `flex-shrink: 0`

### 5.12 Welcome Screen

Shown when no project is open (app launch with no recent project auto-open, or after closing last project). Replaces the gutter + code area — fills the full editor row. The tab bar shows empty (no tabs).

**Layout: two-column**

```
┌──────────────────────┬────────────────────────────────┐
│  Left rail (220px)   │  Right — Recent projects        │
│  border-right 1px    │                                 │
│                      │  [36px header — "RECENT"]       │
│  Faber               │  ─────────────────────────────  │
│  v0.1.0-dev          │  [project item 46px]            │
│                      │  [project item 46px]            │
│  START               │  [project item 46px]            │
│  New Project   ⌘N    │  [project item 46px]            │
│  Open Folder…  ⌘O    │                                 │
│  Clone Repo…         │                                 │
└──────────────────────┴────────────────────────────────┘
```

**Left rail:**
- Width: `220px`, `flex-shrink: 0`
- Border-right: `1px border`
- Padding: `28px 20px 24px`
- Background: `bg`

Brand block (top):
- Wordmark: `"Faber"`, `17px`, weight `700`, letter-spacing `-0.03em`, `text`
- Version tag: `10.5px` monospace, `text_subtle`, margin-top `5px`
- Margin-bottom from brand to section: `28px`

Section label: `10px`, weight `600`, letter-spacing `0.08em`, uppercase, `text_subtle`, padding `0 8px`, margin-bottom `4px`

Action rows:
- Height: `30px`, padding `0 8px`, `border-radius: 7px`
- Font: `12.5px`, color `text_muted`
- Gap between icon and label: `9px`
- Icon: `12px`, `width: 14px`, centered — use simple Unicode: `+` (new), `↗` (open), `⌕` (clone)
- Hover: background `rgba(255,255,255,0.06)`, color `text`
- Keybinding: right-aligned, uses keybinding chips (§5.11)
- Rows: `"New Project"` (`⌘N`), `"Open Folder…"` (`⌘O`), `"Clone Repository…"` (no binding)
- Margin-bottom between actions section and bottom of rail: auto (pushes brand to top)

**Right column:**
- Flex: `1`, overflow hidden
- Header strip: `36px`, border-bottom `1px border`, padding `0 20px`
  - Label: `10.5px`, weight `600`, letter-spacing `0.08em`, uppercase, `text_subtle`
  - Text: `"RECENT"`
- List: padding `8px 10px`, scrollable, no scrollbar

Project item rows:
- Height: `46px`, padding `0 10px`, `border-radius: 9px`
- Gap: `12px`
- Hover: background `rgba(255,255,255,0.05)`
- Click: opens that project (triggers the same path as `"Open Folder…"` with the stored path)

Project item icon:
- Size: `30×30px`, `border-radius: 7px`
- Background: `bg_raised` by default; optionally tinted per project (stored as metadata)
- Content: 2-letter initials of project name, `11px`, weight `600`, sans-serif, `text_muted`

Project item meta:
- Name: `13px`, weight `500`, `text`, truncated
- Path: `11px` monospace, `text_subtle`, truncated — show `~/`-abbreviated path
- Timestamp: `11px`, `text_subtle`, right-aligned, `font-variant-numeric: tabular-nums`

### 5.11 Keybinding Chips

Used in command palette items and welcome screen actions.

- Container: `flex`, `gap: 2px`
- Each key: `display: inline-block`, background `bg_raised`, border `1px border_focus`, `border-radius: 5px`, `padding: 1px 5px`, `font-size: 10px`, `color: text_muted`, line-height `1.5`
- Modifier keys: `⌘` `⌃` `⌥` `⇧` — use Unicode symbols, not words

---

## 6. Interaction Patterns

### 6.1 Overlay stacking and dismissal

Z-index ordering (lowest to highest):
1. Editor content (no z-index / flow)
2. Sidebar (`z: 40`) — dismiss: click outside, `Esc`, `⌘1`
3. Hover popover (`z: 50`) — dismiss: cursor move, `Esc`
4. Autocomplete (`z: 55`) — dismiss: `Esc`, click outside, accept item
5. Command palette (`z: 60`) — dismiss: `Esc`, click outside, select item

Only one glass overlay should be visible at a time. When one opens, close any others at lower z-levels (e.g., opening palette dismisses sidebar; autocomplete dismisses hover popover).

### 6.2 Tab state machine

```
App state → tab bar contents
──────────────────────────────
No project open        → empty tab bar, welcome screen fills editor row
File tab active        → gutter + code visible, ppage hidden
Problems tab active    → gutter hidden, code hidden, problems ppage visible
References tab active  → gutter hidden, code hidden, references ppage visible
```

File tabs persist in the bar. Problems/References tabs are dynamically added/removed.
Last active file tab is remembered; closing a page-tab restores it.

### 6.3 Bottom panel toggle

```
⌘J pressed:
  if closed → height: 0 → 168px (transition)
  if open   → height: 168px → 0 (transition)
```

The panel remembers its last height for a future "resizable" upgrade, but for this pass the height is fixed at `168px`.

### 6.4 Sidebar behavior

- `⌘1` toggles open/closed
- Click anywhere in the editor area (gutter or code) while sidebar is open → closes sidebar
- `Esc` while sidebar is focused → closes sidebar
- Sidebar does not push editor content — it overlays it (zero layout shift)

### 6.5 Entry animations summary

| Surface | Entry | Duration | Easing |
|---|---|---|---|
| Sidebar | `translateX(-100%)` → `translateX(0)` | `220ms` | `cubic-bezier(0.25,0.46,0.45,0.94)` |
| Command palette | `translateY(-12px) scale(0.97)` → `translateY(0) scale(1)` | `220ms` | `cubic-bezier(0.34,1.36,0.64,1)` |
| Hover popover | `opacity 0, translateY(8px)` → `opacity 1, translateY(0)` | `160ms` | `ease` |
| Bottom panel | `height: 0` → `height: 168px` | `200ms` | `cubic-bezier(0.25,0.46,0.45,0.94)` |
| Autocomplete | instant (no animation; appears below cursor immediately) | — | — |

---

## 7. Typography

- **UI font:** `-apple-system, BlinkMacSystemFont, "SF Pro Text", system-ui, sans-serif`
- **Code font:** `"SF Mono", "Menlo", "JetBrains Mono", monospace`
- **Font smoothing:** `-webkit-font-smoothing: antialiased` on all surfaces

Type scale used across the UI:

| Role | Size | Weight | Line height | Usage |
|---|---|---|---|---|
| Display | `17px` | `700` | `1.2` | Welcome screen wordmark |
| Heading | `13px` | `600` | `1.4` | Panel titles, section headers |
| Body | `13px` | `400` | `1.5` | Result names, file tree, palette items |
| Caption | `12px` | `400` | `1.5` | Meta text, locations, type annotations |
| Label | `10–11px` | `600` | `1.5` | Section labels (uppercase), status bar, badges |
| Code | `13px` | `400` | `21px` | Editor code |
| Code small | `12.5px` | `400` | `1.8` | Terminal, autocomplete names, hover signature |
| Code caption | `12px` | `400` | `1.6` | Gutter line numbers |

---

## 8. Spacing Scale

The existing `sp1`–`sp8` scale is retained. Reference values for this design:

| Var | Value | Typical usage |
|---|---|---|
| `sp1` | `2px` | Tight gaps, badge padding |
| `sp2` | `4px` | Inner padding small components |
| `sp3` | `6px` | Button padding, gap in groups |
| `sp4` | `8px` | Row padding, icon margins |
| `sp5` | `10px` | Panel item padding |
| `sp6` | `12px` | Section padding |
| `sp7` | `16px` | Content area padding (top) |
| `sp8` | `20px` | Horizontal content padding |

---

## 9. Radii Scale

| Token | Value | Usage |
|---|---|---|
| `radius_xs` | `4px` | Kind badges, key chips, inline code |
| `radius_sm` | `6px` | Status bar items, small tags |
| `radius_md` | `8–9px` | List rows, palette items, project items |
| `radius_lg` | `10px` | Popovers, autocomplete, bottom panel corners |
| `radius_xl` | `14px` | Command palette |

---

## 10. GPUI Implementation Map

### Files to modify

| File | Change |
|---|---|
| `crates/faber-theme/src/lib.rs` | Replace Catppuccin palette with OLED Black tokens (§2). Add `bg_raised`, `bg_sunken` to `SemanticColors`. Update `Radii` to match §9. |
| `crates/faber-app/src/workspace.rs` | Implement welcome screen (§5.12) as a state returned when `pane_group` has no open documents. Add glass sidebar overlay (§5.4) positioned absolute. Rework bottom panel (§5.10): remove drag handle, remove close button, fix height to `168px`. |
| `crates/faber-app/src/pane.rs` | `TabKind::Problems` and `TabKind::References` are page-tabs: when active, render the page view instead of (not alongside) gutter+code. Add Problems/References to `TabKind` if not present. |
| `crates/faber-app/src/editor_view.rs` | Apply new token values. Implement autocomplete dropdown (§5.7) as a glass surface anchored below caret. Wire up `textDocument/completion` (LSP backlog C4). |
| `crates/faber-app/src/panels/references_panel.rs` | Restyle as full page-tab view matching §5.9. |
| `crates/faber-app/src/panels/diagnostics_panel.rs` (or equivalent) | Restyle as full page-tab view matching §5.8. |
| `crates/faber-app/locales/en.toml` | Add keys for welcome screen strings: `welcome.new_project`, `welcome.open_folder`, `welcome.clone_repo`, `welcome.recent`, `welcome.no_recent`. |

### Do not change

- `faber-core`, `faber-editor`, `faber-index`, `faber-settings` — no UI changes
- LSP plumbing (`faber-lsp`) — this spec only affects rendering
- Existing `TabItem` / `TabKind` architecture — extend, do not replace
- `Document::apply(Transaction)` pipeline — untouched
- Keybindings beyond what is specified here

---

## 11. Reference: Prototype

The interactive HTML prototype that this spec was derived from is at:
`/private/tmp/claude-501/-Users-rodrigo-Codes-ellist-faber/.../faber-prototype.html`
Published at: https://claude.ai/code/artifact/f18e2f17-60fb-4920-9904-fb14d36c9a47

The prototype is the ground truth for visual feel. When a measurement here is ambiguous,
inspect the prototype CSS. Prototype CSS class → spec section mapping:

| CSS class | Spec section |
|---|---|
| `.glass` | §3 |
| `.tb` | §5.1 |
| `.tabbar`, `.tab` | §5.2 |
| `.gut`, `.cs`, `.ln` | §5.3 |
| `.sidebar` | §5.4 |
| `.pscrim`, `.palette` | §5.5 |
| `.hov` | §5.6 |
| `.ac` | §5.7 |
| `.ppage` | §5.8 |
| `.botpanel` | §5.10 |
| `.stb`, `.si` | §5.11 |
| `.wlpane` | §5.12 |
