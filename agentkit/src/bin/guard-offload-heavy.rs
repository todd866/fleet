fn main() {
    agentkit::quiet_sigpipe();
    std::process::exit(agentkit::guard_main("offload-heavy"));
}
