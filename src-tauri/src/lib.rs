mod config;
mod i18n;
mod paths;

use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
  tauri::Builder::default()
    .setup(|app| {
      if cfg!(debug_assertions) {
        app.handle().plugin(
          tauri_plugin_log::Builder::default()
            .level(log::LevelFilter::Info)
            .build(),
        )?;
      }

      let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
      let menu = Menu::with_items(app, &[&quit])?;

      TrayIconBuilder::new()
        .icon(tauri::image::Image::from_bytes(include_bytes!(
          "../../assets/tray.png"
        ))?)
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| {
          if event.id.as_ref() == "quit" {
            app.exit(0);
          }
        })
        .build(app)?;

      Ok(())
    })
    .run(tauri::generate_context!())
    .expect("error while running tauri application");
}
