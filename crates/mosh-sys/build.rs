fn main() {
    // Mirrors configure.ac: prefer tinfo, fall back to ncurses, then curses.
    let terminfo = ["tinfo", "ncursesw", "ncurses"]
        .iter()
        .any(|name| pkg_config::probe_library(name).is_ok());
    if !terminfo {
        println!("cargo:rustc-link-lib=curses");
    }

    // libutempter ships no pkg-config file, so look for its header the way configure.ac
    // does. A Cargo feature cannot be turned on from here, hence the cfg.
    println!("cargo::rustc-check-cfg=cfg(has_utempter)");
    let header = ["/usr/include/utempter.h", "/usr/local/include/utempter.h"]
        .into_iter()
        .find(|p| std::path::Path::new(p).exists());
    // Watching a path that is not there yet re-runs this script until the package appears.
    println!(
        "cargo::rerun-if-changed={}",
        header.unwrap_or("/usr/include/utempter.h")
    );
    println!("cargo::rerun-if-env-changed=CARGO_FEATURE_UTEMPTER");
    if header.is_some() || std::env::var_os("CARGO_FEATURE_UTEMPTER").is_some() {
        println!("cargo:rustc-link-lib=utempter");
        println!("cargo:rustc-cfg=has_utempter");
    }
}
