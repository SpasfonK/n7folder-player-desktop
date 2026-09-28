fn main() {
    // Manifeste Windows personnalisé : chemins longs (> 260), UTF-8, DPI par moniteur, Windows 10/11.
    let windows = tauri_build::WindowsAttributes::new()
        .app_manifest(include_str!("windows/app.manifest"));
    tauri_build::try_build(tauri_build::Attributes::new().windows_attributes(windows))
        .expect("échec de tauri-build");
}
