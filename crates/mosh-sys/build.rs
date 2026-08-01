fn main() {
    // Mirrors configure.ac: prefer tinfo, fall back to ncurses, then curses.
    for name in ["tinfo", "ncursesw", "ncurses"] {
        if pkg_config::probe_library(name).is_ok() {
            return;
        }
    }
    println!("cargo:rustc-link-lib=curses");
}
