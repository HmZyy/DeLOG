use std::path::{Path, PathBuf};

pub fn bundled_python_home(exe_path: &Path) -> PathBuf {
    exe_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("python")
}

pub fn init_bundled_python() {
    let exe = std::env::current_exe().expect("current_exe() failed");
    let home = bundled_python_home(&exe);
    if !home.is_dir() {
        eprintln!(
            "fatal: bundled Python runtime not found at {}\n\
             This install is incomplete - reinstall DeLOG.",
            home.display()
        );
        std::process::exit(1);
    }
    // SAFETY: called at the very top of main() while single-threaded, before any
    // other thread is spawned and before CPython initializes.
    unsafe {
        std::env::set_var("PYTHONHOME", &home);
        std::env::remove_var("PYTHONPATH");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_is_python_dir_beside_exe() {
        let home = bundled_python_home(Path::new("/opt/delog/bin/delog"));
        assert_eq!(home, PathBuf::from("/opt/delog/bin/python"));
    }

    #[test]
    fn home_handles_bare_filename() {
        let home = bundled_python_home(Path::new("delog"));
        assert_eq!(home, PathBuf::from("python"));
    }
}
