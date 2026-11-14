fn main() {
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let code = diskmap::run(std::env::args().collect());
    std::process::exit(code);
}
