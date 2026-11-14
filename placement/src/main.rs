fn main() {
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let code = placement::run(std::env::args().collect());
    std::process::exit(code);
}
