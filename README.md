# gpui-tray

Cross-platform system tray support for apps using upstream `gpui` (from the Zed repository), without modifying `gpui`.

## Notes

- The `gpui` dependency is pulled from Zed by tag.
- On macOS, this repo pins `core-text` to match Zed's lockfile expectations; otherwise Cargo may resolve versions that break `zed-font-kit`.

## Run Demo

```bash
cargo run --example tray_demo
```

Tray debug log (macOS): `/tmp/gpui_tray_demo_tray.log`

## Platforms

- macOS: implemented via AppKit (`NSStatusItem`).
- Windows: implemented via Win32 tray icon (`Shell_NotifyIconW`) and a hidden window.
- Linux: implemented via StatusNotifierItem (DBus) using `ksni`.
