use clap::Parser;
fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    // Clap owns help/version and usage exit codes; execution errors use the response budget.
    let options = trufflepig::cli::Arguments::parse();
    match trufflepig::cli::run(&args) {
        Ok(response) => print!("{response}"),
        Err(error) => {
            let message = format!("{error:#}");
            if let Ok(budget) = trufflepig::output::OutputBudget::new(options.budget)
                && let Ok(response) = budget.render(&serde_json::json!({"error":message}))
            {
                print!("{response}");
            }
            eprintln!("{}", message.replace(['\n', '\r'], " "));
            std::process::exit(2);
        }
    }
}
