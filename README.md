# gpui-tray

Cross-platform system tray support for apps using upstream `gpui` (from the Zed repository), without modifying `gpui`.

## Use

Add the dependency:

```toml
[dependencies]
gpui-tray = { git = "https://github.com/Tryanks/gpui_tray" }
```

Create and install a tray item:

```rust
use gpui::{App, Application};
use gpui_tray::{TrayEvent, TrayItem, TrayMenuItem};

fn main() -> anyhow::Result<()> {
    Application::new().run(|cx: &mut App| {
        let async_app = cx.to_async();

        let item = TrayItem::new()
            .visible(true)
            .icon(gpui::Image::from_bytes(
                gpui::ImageFormat::Png,
                include_bytes!("app-icon.png").to_vec(),
            ))
            .title("My App")
            .tooltip("Hello from tray")
            .submenu(TrayMenuItem::menu("quit", "Quit", Vec::new()))
            .on_event(|event, cx| match event {
                TrayEvent::MenuClick { id } if id == "quit" => cx.quit(),
                _ => {}
            });

        gpui_tray::tray::set_up_tray(cx, async_app, item).ok();
    });
    Ok(())
}
```

Update the tray later by calling `gpui_tray::tray::sync_tray(cx, item)` with a new `TrayItem`.

### Icon Notes

- `.icon(...)` takes anything convertible into `gpui::Image` (e.g. `gpui::Image::from_bytes(...)`).

## Run Demo

```bash
cargo run --example tray_demo
```
