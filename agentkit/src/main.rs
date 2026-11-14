use std::env;
use std::io::Write;
use std::process;

fn main() {
    agentkit::quiet_sigpipe();
    let _ = std::io::stdout().flush();
    process::exit(agentkit::run(&env::args().collect::<Vec<_>>()));
}
