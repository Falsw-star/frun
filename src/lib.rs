use std::path::PathBuf;

use directories::ProjectDirs;

pub mod protocal;
pub mod daemon;

pub struct Paths {
    pub daemon_info_path: PathBuf
}

pub fn get_paths() -> Paths {
    let dirs = ProjectDirs::from(
        "dev",
        "false_star",
        "backrunner"
    ).expect("Cannot determine directories");
    Paths {
        daemon_info_path: dirs.data_dir().join("daemon_info.json")
    }
}