# jterm2

A modern, GPU-accelerated terminal emulator written in Rust with egui.

## Features

- **GPU-accelerated rendering** - Hardware-accelerated text rendering using wgpu
- **Kitty graphics protocol** - Display images directly in the terminal
- **Multi-session support** - Multiple terminal sessions with tab management
- **Search & Replace** - Powerful text search with context display
- **Session persistence** - Automatic save/restore of sessions
- **Customizable themes** - Full theme support with multiple built-in themes
- **Font configuration** - Flexible font selection with fallback support
- **Command palette** - Quick access to all features
- **Performance monitoring** - Built-in debug overlay with FPS, memory usage
- **VTE compatibility** - Full ANSI/VTE escape sequence support

## Performance Optimizations

- LRU texture cache for Kitty graphics (prevents unbounded memory growth)
- Dirty-region rendering (only redraws changed cells)
- Frame budget system (limits processing per frame to maintain 60 FPS)
- Zero-copy rendering with Arc-based instance buffers
- Keyboard input buffer reuse (reduces allocations)
- Smart cursor blinking (only when idle)

## Building

### Prerequisites

- Rust 1.70 or later
- Linux (tested on Ubuntu/Debian)

### Compile

```bash
# Debug build (faster compilation)
cargo build

# Release build (optimized)
cargo build --release
```

### Run

```bash
# Debug
cargo run

# Release
cargo run --release
```

## Configuration

Configuration file: `~/.config/jterm2/config.toml`

Key settings:
- `font_size` - Base font size
- `font_family` - Primary font family
- `font_fallback` - Fallback fonts for Unicode
- `theme` - Color scheme name
- `ui_scale` - UI scaling factor
- `scroll_speed` - Mouse wheel scroll speed

## Keybindings

- `Ctrl+Shift+T` - New tab
- `Ctrl+Shift+W` - Close tab
- `Ctrl+Shift+F` - Search
- `Ctrl+Shift+P` - Command palette
- `Ctrl+Shift+,` - Settings panel
- `F12` - Debug overlay
- `Ctrl+Tab` / `Ctrl+Shift+Tab` - Switch tabs
- `Ctrl+Shift+C` - Copy
- `Ctrl+Shift+V` - Paste

## Architecture

### Core Components

- **main.rs** - Application entry point, main event loop
- **src/terminal/mod.rs** - VTE state machine, ANSI escape sequence parsing
- **src/ui.rs** - GPU-accelerated renderer, UI layout
- **src/shell.rs** - PTY management, subprocess handling
- **src/session_manager.rs** - Multi-session coordination
- **src/theme.rs** - Color scheme system
- **gpu/** - WGPU rendering pipeline

### Rendering Pipeline

1. VTE parser updates grid state (`src/terminal/mod.rs`)
2. Dirty-region detection compares grid versions (`src/ui.rs`)
3. Changed cells compiled to GPU instances (`src/gpu/instance.rs`)
4. Instances uploaded to vertex buffer
5. WGPU draws using instanced rendering (`src/gpu/pipeline.rs` and `src/gpu/callback.rs`)

### Performance Design

- **Frame budget**: Max 32KB ANSI data per frame (5ms processing)
- **Dirty tracking**: Only rebuild changed rows
- **Cache coherence**: Quantized subpixel positioning for glyph cache
- **Batch rendering**: Single draw call for entire grid

## Development

### Debug Build

Development builds include:
- Incremental compilation
- Dependency optimization (opt-level=1)
- Fast compile times

### Release Build

Release builds enable:
- Thin LTO (Link Time Optimization)
- Symbol stripping
- Single codegen unit (maximum optimization)
- opt-level=3

### Testing

```bash
# Check compilation
cargo check

# Run lints
cargo clippy

# Format code
cargo fmt
```

## License

See LICENSE file for details.

## Contributing

Contributions welcome! Please ensure:
- Code passes `cargo clippy`
- Format with `cargo fmt`
- Test changes with both debug and release builds
