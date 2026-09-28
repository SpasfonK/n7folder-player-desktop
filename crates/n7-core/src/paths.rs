//! Emplacement des données de l'application : `%APPDATA%\n7FolderPlayer`.

use std::ffi::OsString;
use std::io;
use std::path::PathBuf;

/// Nom du dossier de données, sous `%APPDATA%`.
pub const APP_DIR_NAME: &str = "n7FolderPlayer";

/// Dossier de données de l'application (non créé par cette fonction).
///
/// Windows : `%APPDATA%\n7FolderPlayer`. Sur Linux/macOS (développement uniquement) :
/// `$XDG_CONFIG_HOME/n7FolderPlayer` ou `~/.config/n7FolderPlayer`.
pub fn data_dir() -> io::Result<PathBuf> {
    resolve_base(env_var)
        .map(|base| base.join(APP_DIR_NAME))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "dossier de données utilisateur introuvable (variable APPDATA absente)",
            )
        })
}

fn env_var(key: &str) -> Option<OsString> {
    std::env::var_os(key)
}

fn resolve_base<F>(env: F) -> Option<PathBuf>
where
    F: Fn(&str) -> Option<OsString>,
{
    let non_empty = |key: &str| env(key).filter(|v| !v.is_empty()).map(PathBuf::from);
    non_empty("APPDATA")
        .or_else(|| non_empty("XDG_CONFIG_HOME"))
        .or_else(|| non_empty("HOME").map(|home| home.join(".config")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<OsString> {
        move |key| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| OsString::from(*v))
        }
    }

    #[test]
    fn appdata_has_priority() {
        let env = fake_env(&[("APPDATA", "C:\\Users\\x\\AppData\\Roaming"), ("HOME", "/home/x")]);
        assert_eq!(
            resolve_base(env),
            Some(PathBuf::from("C:\\Users\\x\\AppData\\Roaming"))
        );
    }

    #[test]
    fn falls_back_to_xdg_then_home() {
        assert_eq!(
            resolve_base(fake_env(&[("XDG_CONFIG_HOME", "/cfg"), ("HOME", "/home/x")])),
            Some(PathBuf::from("/cfg"))
        );
        assert_eq!(
            resolve_base(fake_env(&[("HOME", "/home/x")])),
            Some(PathBuf::from("/home/x").join(".config"))
        );
    }

    #[test]
    fn empty_values_are_ignored() {
        assert_eq!(
            resolve_base(fake_env(&[("APPDATA", ""), ("HOME", "/home/x")])),
            Some(PathBuf::from("/home/x").join(".config"))
        );
        assert_eq!(resolve_base(fake_env(&[])), None);
    }
}
