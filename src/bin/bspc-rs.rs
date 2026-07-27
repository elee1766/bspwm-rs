use std::io;

fn main() {
    const FAILURE: i32 = 1;

    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments.is_empty() {
        eprintln!("No arguments given.");
        std::process::exit(FAILURE);
    }
    let Some(socket_path) = bspwm_client::socket_path_from_env() else {
        eprintln!("Failed to determine the socket path.");
        std::process::exit(FAILURE);
    };
    if arguments[0] == "--print-socket-path" {
        println!("{}", socket_path.display());
        return;
    }

    let stdout = io::stdout();
    let stderr = io::stderr();
    match bspwm_client::send_message_stream(
        &socket_path,
        &arguments,
        &mut stdout.lock(),
        &mut stderr.lock(),
    ) {
        Ok(false) => {}
        Ok(true) => std::process::exit(FAILURE),
        Err(_) => {
            eprintln!("Failed to connect to the socket.");
            std::process::exit(FAILURE);
        }
    }
}
