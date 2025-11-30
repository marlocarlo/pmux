# pmux

A PowerShell-focused terminal multiplexer inspired by tmux, written in Rust.

## Features

- Split panes horizontally and vertically
- Multiple windows with tabs
- Session management (attach/detach)
- Mouse support for resizing panes
- Copy mode with vim-like keybindings
- Synchronized input to multiple panes
- Cross-platform (Windows, Linux, macOS)

## Installation

### From Binary Release

Download the latest release from [GitHub Releases](https://github.com/computersrmyfriends/pmux/releases).

### From Source

```bash
cargo install --path .
```

Or build manually:

```bash
cargo build --release
# Binary will be at target/release/pmux (or pmux.exe on Windows)
```

### Using Cargo

```bash
cargo install pmux
```

## Usage

```bash
# Start a new session
pmux

# Start a named session
pmux new-session -s mysession

# List sessions
pmux ls

# Attach to a session
pmux attach -t mysession
```

## Key Bindings

The default prefix key is `Ctrl+b` (like tmux).

| Key | Action |
|-----|--------|
| `Prefix + c` | Create new window |
| `Prefix + %` | Split pane left/right (horizontal) |
| `Prefix + "` | Split pane top/bottom (vertical) |
| `Prefix + x` | Kill current pane |
| `Prefix + z` | Toggle pane zoom |
| `Prefix + n` | Next window |
| `Prefix + p` | Previous window |
| `Prefix + 0-9` | Select window by number |
| `Prefix + d` | Detach from session |
| `Prefix + ,` | Rename current window |
| `Prefix + w` | Window/pane chooser |
| `Prefix + [` | Enter copy mode |
| `Prefix + ]` | Paste from buffer |
| `Prefix + q` | Display pane numbers |
| `Prefix + Arrow` | Navigate between panes |
| `Ctrl+q` | Quit pmux |

## Configuration

Create a config file at `~/.pmux.conf`:

```
# Change prefix key to Ctrl+a
set -g prefix C-a

# Enable mouse
set -g mouse on

# Customize status bar
set -g status-left "[#S]"
set -g status-right "%H:%M"

# Cursor style: block, underline, or bar
set -g cursor-style bar
set -g cursor-blink on
```

## License

MIT