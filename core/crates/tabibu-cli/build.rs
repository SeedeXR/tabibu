//! Generate the `tabibu.1` man page from the clap definition at build time, so
//! the man page always matches the actual `--help`. Output goes to `OUT_DIR`
//! (packaging/install picks it up); a committed snapshot lives in `man/` for
//! viewing in the repo (refresh it with `scripts/gen-man.sh`).

use clap::CommandFactory;

// The SAME command definition the binary parses (see the module doc in cli.rs).
include!("src/cli.rs");

fn main() {
    println!("cargo:rerun-if-changed=src/cli.rs");
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR set by cargo"));
    let mut buf = Vec::new();
    clap_mangen::Man::new(Cli::command())
        .render(&mut buf)
        .expect("render man page");
    std::fs::write(out.join("tabibu.1"), &buf).expect("write tabibu.1 to OUT_DIR");
}
