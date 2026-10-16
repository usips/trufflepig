fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let mut stdout = std::io::stdout();
    let mut stderr = std::io::stderr();
    let code = trufflepig::cli::emission::execute(&args, &mut stdout, &mut stderr);
    std::process::exit(code);
}
