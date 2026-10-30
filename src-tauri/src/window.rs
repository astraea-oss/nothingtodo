//! Main window, tray and close-to-tray behaviour. Reminders only fire while the process runs,
//! so on desktop closing the window hides it to the tray instead of quitting.
use crate::paths::AppPaths;
use tauri::AppHandle;
#[cfg(desktop)]
use tauri::Manager;

const MAIN: &str = "main";

#[cfg(desktop)]
/// Autostart passes this so the app starts in the tray rather than opening a window.
pub fn start_hidden() -> bool {
    std::env::args().any(|arg| arg == "--hidden")
}

pub fn create_main(app: &AppHandle, paths: &AppPaths, visible: bool) -> tauri::Result<()> {
    let config = app
        .config()
        .app
        .windows
        .iter()
        .find(|window| window.label == MAIN)
        .ok_or_else(|| tauri::Error::WindowNotFound)?
        .clone();
    let builder = tauri::WebviewWindowBuilder::from_config(app, &config)?;
    // Keep WebKit's data with the portable profile instead of the per-user default.
    #[cfg(desktop)]
    let builder = builder.data_directory(paths.webview_dir()).visible(visible);
    #[cfg(mobile)]
    let _ = (paths, visible);
    builder.build()?;
    Ok(())
}

#[cfg(desktop)]
pub fn show_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

#[cfg(desktop)]
pub fn create_tray(app: &AppHandle) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

    let open = MenuItem::with_id(app, "open", "Open NothingToDo", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit (reminders stop)", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &quit])?;
    let mut tray = TrayIconBuilder::with_id(MAIN)
        .tooltip("NothingToDo")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_main(app),
            "quit" => app.exit(0),
            _ => {}
        })
        // Linux trays deliver the menu only; click events cover other desktops.
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

/// Hides instead of closing when a tray icon exists to bring the window back. Launching the app
/// again also shows it, through the single-instance handler.
#[cfg(desktop)]
pub fn on_event(window: &tauri::Window, event: &tauri::WindowEvent) {
    if let tauri::WindowEvent::CloseRequested { api, .. } = event
        && window.label() == MAIN
        && window.app_handle().tray_by_id(MAIN).is_some()
    {
        api.prevent_close();
        let _ = window.hide();
    }
}
