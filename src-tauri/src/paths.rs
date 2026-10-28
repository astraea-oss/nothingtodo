//! Where the profile lives. Desktop builds are portable: data stays in a folder beside the
//! executable (or beside the AppImage file), so moving the folder moves the tasks. Android uses
//! its private app storage.
use std::path::{Path, PathBuf};

#[cfg(not(target_os = "android"))]
const PORTABLE_DIRECTORY_NAME: &str = "NothingToDo Data";

pub struct AppPaths {
    pub data_dir: PathBuf,
}

impl AppPaths {
    pub fn resolve(app: &tauri::AppHandle) -> Result<Self, String> {
        #[cfg(target_os = "android")]
        let data_dir = {
            use tauri::Manager;
            app.path().app_data_dir().map_err(|e| e.to_string())?
        };
        #[cfg(not(target_os = "android"))]
        let data_dir = {
            let _ = app;
            portable_dir(
                std::env::var_os("APPIMAGE").as_deref(),
                std::env::current_exe(),
            )?
        };
        let paths = Self { data_dir };
        for dir in [&paths.data_dir, &paths.webview_dir()] {
            std::fs::create_dir_all(dir).map_err(|e| unwritable(dir, e))?;
        }
        verify_writable(&paths.data_dir)?;
        Ok(paths)
    }

    pub fn database(&self) -> PathBuf {
        self.data_dir.join("nothingtodo.sqlite3")
    }

    pub fn webview_dir(&self) -> PathBuf {
        self.data_dir.join("webview")
    }
}

fn unwritable(path: &Path, error: impl std::fmt::Display) -> String {
    format!(
        "Cannot write the NothingToDo data folder ({}): {error}. Move the complete app folder somewhere writable, then start NothingToDo again.",
        path.display()
    )
}

fn verify_writable(dir: &Path) -> Result<(), String> {
    let probe = dir.join(".write-test");
    std::fs::write(&probe, b"").map_err(|e| unwritable(dir, e))?;
    let _ = std::fs::remove_file(probe);
    Ok(())
}

#[cfg(not(target_os = "android"))]
fn portable_dir(
    appimage: Option<&std::ffi::OsStr>,
    current_exe: std::io::Result<PathBuf>,
) -> Result<PathBuf, String> {
    // Inside an AppImage the executable runs from a temporary read-only mount.
    let launcher = match appimage.filter(|value| !value.is_empty()) {
        Some(path) if Path::new(path).is_absolute() => PathBuf::from(path),
        Some(path) => {
            return Err(format!(
                "APPIMAGE is not an absolute path: {}",
                Path::new(path).display()
            ));
        }
        None => {
            current_exe.map_err(|e| format!("Cannot locate the NothingToDo executable: {e}"))?
        }
    };
    let folder = launcher
        .parent()
        .ok_or_else(|| format!("{} has no parent folder.", launcher.display()))?;
    Ok(folder.join(PORTABLE_DIRECTORY_NAME))
}

#[cfg(all(test, not(target_os = "android")))]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn data_lives_beside_the_executable_or_appimage() {
        let exe = Ok(PathBuf::from("/opt/NothingToDo/nothingtodo"));
        assert_eq!(
            portable_dir(None, exe).unwrap(),
            Path::new("/opt/NothingToDo/NothingToDo Data")
        );
        let mounted = Ok(PathBuf::from("/tmp/.mount_x/usr/bin/nothingtodo"));
        let appimage = OsStr::new("/home/me/Apps/NothingToDo.AppImage");
        assert_eq!(
            portable_dir(Some(appimage), mounted).unwrap(),
            Path::new("/home/me/Apps/NothingToDo Data")
        );
        assert!(portable_dir(Some(OsStr::new("relative.AppImage")), Ok("/x/y".into())).is_err());
    }
}
