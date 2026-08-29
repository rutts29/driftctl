use std::io::Write;

fn main() {
    let root = match std::env::current_dir() {
        Ok(root) => root,
        Err(error) => {
            eprintln!("could not determine the current directory: {error}");
            std::process::exit(1);
        }
    };
    let arguments = std::env::args().skip(1);
    let output = driftctl::cli::execute(&root, arguments);

    if !output.stdout.is_empty() {
        let _ = writeln!(std::io::stdout().lock(), "{}", output.stdout);
    }
    if !output.stderr.is_empty() {
        let _ = writeln!(std::io::stderr().lock(), "{}", output.stderr);
    }
    std::process::exit(output.exit_code);
}
