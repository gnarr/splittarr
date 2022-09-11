use directories::ProjectDirs;

pub fn dirs() -> ProjectDirs {
    ProjectDirs::from("org", "Gnarr", "Splittarr").unwrap()
}
