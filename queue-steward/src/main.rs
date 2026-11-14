use std::env;
use std::process;

fn main() {
    // Die quietly on a closed pipe (`status | head -1`), as a C program and the bash original do,
    // instead of panicking on EPIPE.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let code = queue_steward::run(env::args().collect());
    process::exit(code);
}
