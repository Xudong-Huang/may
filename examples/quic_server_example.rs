use may::net::quic::QuicServer;

fn main() {
    // Setup a simple logger (optional, but helpful for seeing output)
    // You might need to add `env_logger` to [dev-dependencies] in Cargo.toml
    env_logger::init_from_env(env_logger::Env::default().default_filter_or("info"));

    let server_addr = "127.0.0.1:8088";

    println!("[QUIC Example] Attempting to start QUIC server on {}", server_addr);

    let server = QuicServer::new();

    // The start method will block the main coroutine/thread if called directly like this.
    // In a real application, you might spawn it in its own may coroutine if main needs to do other things.
    if let Err(e) = server.start(server_addr) {
        eprintln!("[QUIC Example] Server failed to start: {}", e);
    }
}