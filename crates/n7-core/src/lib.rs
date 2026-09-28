//! Coeur métier de n7Folder Player.
//!
//! Ce crate ne dépend ni de Tauri ni d'une API graphique : ses tests tournent partout et vite.
//! Le shell Tauri (`src-tauri`) n'est qu'une fine couche de commandes au-dessus.

pub mod cache;
pub mod covers;
pub mod library;
pub mod parse_path;
pub mod paths;
pub mod queue;
pub mod scanner;
pub mod settings;
