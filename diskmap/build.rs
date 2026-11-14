fn main() {
    println!("cargo:rustc-link-lib=sqlite3");
    println!("cargo:rustc-link-lib=framework=CoreFoundation");
    println!("cargo:rustc-link-lib=framework=CoreServices");
}
