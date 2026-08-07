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
    let forced = std::env::var_os("CARGO_FEATURE_UTEMPTER").is_some();
    if forced
        || ["/usr/include/utempter.h", "/usr/local/include/utempter.h"]
            .iter()
            .any(|p| std::path::Path::new(p).exists())
    {
        println!("cargo:rustc-link-lib=utempter");
        println!("cargo:rustc-cfg=has_utempter");
    }
}
