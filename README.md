# KeyFlow

**Status: BETA — not fully functional. Use at your own risk.**

KeyFlow is a global keyboard shortcut remapper written in Rust. It lets you replace any system or application shortcut with your own custom key combination.

## What it does

- Browse hundreds of real shortcuts by category (Windows, Explorer, Browser, VS Code, Office, Design, Terminal, Gaming, General)
- Replace any shortcut with a new key combo — pressing your new combo triggers the original behavior
- Add custom hotkeys for: text, key combos, launching programs, opening URLs, or toggling KeyFlow on/off
- Rules save automatically to `keyflow.toml`
- Runs as Administrator so it can intercept keys in elevated windows
- Tray icon with Pause / Resume

## Current limitations (beta)

- **Ctrl+Alt+Del cannot be intercepted** — this is a Windows security boundary no user-mode tool can cross
- **Some Win+ combinations** (Win+L, etc.) are reserved by the Windows shell and may not be replaceable
- **Elevated/Admin windows** require KeyFlow to run as Administrator
- GUI is minimal and may have layout issues
- Not all key combinations are guaranteed to work in all applications

## Build & run

```bash
cargo build --release
cargo run --release
```

Requires Rust and the MSVC or GNU Windows toolchain.

## License

MIT
